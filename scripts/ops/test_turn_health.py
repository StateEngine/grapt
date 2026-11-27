import contextlib
import hashlib
import importlib.util
import io
import json
import os
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

spec = importlib.util.spec_from_file_location(
    "turn_health", Path(__file__).with_name("turn_health.py"))
turn_health = importlib.util.module_from_spec(spec)
spec.loader.exec_module(turn_health)

UNTIL = "2026-01-02T00:00:00Z"
UNTIL_MS = 1767312000000
WINDOW_START_MS = 1767290400000  # 2026-01-01T18:00:00Z


def dispatch_line(at_ms):
    return ("SECODE_PROVIDER_DISPATCH attempt=x01 at_ms={at} "
            "response_head_at_ms={at} first_stream_byte_at_ms={at} "
            "http_status=200 request_body_bytes=1 tools_bytes=1").format(at=at_ms)


def step_failed(session, reason, detail, at_ms=None):
    line = ("step-failed session={s} step=7 reason={r} detail={d}").format(
        s=session, r=reason, d=detail)
    return dispatch_line(at_ms) + "\n" + line if at_ms else line


def make_seat(root):
    seat = root / "seat"
    (seat / "logs").mkdir(parents=True)
    return seat


def write_log(seat, name, lines):
    (seat / "logs" / name).write_text("\n".join(lines) + "\n", encoding="utf-8")


def add_events(seat, events):
    con = sqlite3.connect(str(seat / "database.sqlite3"))
    con.execute(
        "CREATE TABLE event (id TEXT PRIMARY KEY, aggregate_family TEXT, "
        "aggregate_id TEXT, seq INTEGER, created INTEGER, type TEXT, data TEXT)")
    con.executemany(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, "
        "type, data) VALUES (?, 'session', ?, 1, ?, ?, ?)",
        [("ev_{:04d}".format(i), aid, created, etype, data)
         for i, (etype, created, aid, data) in enumerate(events)])
    con.commit()
    con.close()


def admitted(created, session="ses_aaa111", input_id=None):
    data = {"input": {"data": {"description": "wake"}},
            "sessionID": session}
    if input_id is not None:
        data["inputID"] = input_id
    return ("session.input.admitted.1", created, session,
            json.dumps(data))


def promoted(created, session="ses_aaa111", input_id=None):
    return ("session.input.promoted.1", created, session,
            json.dumps({"sessionID": session, "inputID": input_id}))


def started(created, session="ses_aaa111", execution="exe_1"):
    return ("session.execution.started.1", created, session,
            json.dumps({"executionID": execution, "sessionID": session}))


def succeeded(created, session="ses_aaa111", execution="exe_1"):
    return ("session.execution.succeeded.1", created, session,
            json.dumps({"executionID": execution}))


def exec_failed(created, session="ses_aaa111", execution="exe_1",
                message="failed"):
    return ("session.execution.failed.1", created, session,
            json.dumps({"executionID": execution,
                        "error": {"type": "ExecutionFailed",
                                  "message": message}}))


def db_step_failed(created, session, reason_obj):
    return ("session.step.failed.3", created, session,
            json.dumps({"completedAt": created, "reason": reason_obj, "step": 7}))


def db_interrupted(created, session, reason, execution="exe_1"):
    return ("session.execution.interrupted.1", created, session,
            json.dumps({"executionID": execution, "reason": reason,
                        "sessionID": session}))


def run_main(seat, *extra):
    out = io.StringIO()
    with mock.patch.object(turn_health, "find_service",
                           return_value=(4242, "/fake/secode-server", None)), \
            contextlib.redirect_stdout(out):
        code = turn_health.main(["--seat", str(seat), "--until", UNTIL, *extra])
    return code, out.getvalue()


class ScanLogTests(unittest.TestCase):
    def test_attribution_carry_forward_and_unattributed(self):
        seat = None  # scan_logs takes paths, not a seat
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            lines = [
                "service lifetime=service-unlimited",  # no epoch yet
                dispatch_line(WINDOW_START_MS + 1000),
                step_failed("ses_aaa111",
                            "ProviderFailure { class: LocalRefusal }",
                            "the attempt plan was refused: read capacity is "
                            "busy; retry the request"),
                "native mail retry delay_ms=250",  # inherits the epoch
            ]
            write_log = tmp / "server.log"
            write_log.write_text("\n".join(lines) + "\n")
            state = turn_health.scan_logs([write_log], WINDOW_START_MS, UNTIL_MS)
        self.assertEqual(state["lines_total"], 4)
        self.assertEqual(state["unattributed"], 1)
        self.assertEqual(state["lines_in_window"], 3)
        self.assertEqual(
            state["failures"],
            {"provider-failure/local-refusal/read-capacity-busy": 1})
        self.assertEqual(state["per_session"], {"ses_aaa111": [1, WINDOW_START_MS + 1000]})

    def test_out_of_range_timestamp_token_is_ignored_by_epoch_scanner(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            lines = [
                "timestamp-out-of-range WARN clock out of range",
                dispatch_line(WINDOW_START_MS + 1000),
                "timestamp-out-of-range WARN still out of range",
                step_failed("ses_aaa111",
                            "ProviderFailure { class: LocalRefusal }",
                            "the attempt plan was refused: read capacity is "
                            "busy; retry the request"),
            ]
            write_log = tmp / "server.log"
            write_log.write_text("\n".join(lines) + "\n")
            state = turn_health.scan_logs([write_log], WINDOW_START_MS, UNTIL_MS)
        self.assertEqual(state["unattributed"], 1)
        self.assertEqual(state["first"], WINDOW_START_MS + 1000)
        self.assertEqual(state["last"], WINDOW_START_MS + 1000)
        self.assertEqual(
            state["failures"],
            {"provider-failure/local-refusal/read-capacity-busy": 1})

    def test_detail_keys_and_generic_slug(self):
        cls, flag = turn_health.classify_reason(
            "ProviderFailure { class: LocalRefusal }",
            "the attempt plan was refused: read capacity is busy; retry the request")
        self.assertEqual(cls, "provider-failure/local-refusal/read-capacity-busy")
        self.assertFalse(flag)
        cls, flag = turn_health.classify_reason(
            "ProviderFailure { class: LocalRefusal }",
            "staging: provider request exceeds its item or tool count bound")
        self.assertEqual(cls, "provider-failure/local-refusal/item-tool-count-bound")
        cls, flag = turn_health.classify_reason(
            "ContextFailure { failure: CompactionFailed }",
            "required context preparation failed: CompactionFailed")
        self.assertEqual(cls, "context-failure/compaction-failed")
        cls, flag = turn_health.classify_reason(
            "Interrupted { by: ManagedShutdown }", "attempt interrupted")
        self.assertEqual(cls, "interrupted/managed-shutdown")
        self.assertTrue(flag)
        cls, flag = turn_health.classify_reason("Weird { x: 1 }", "boom")
        self.assertEqual(cls, "weird/1")

    def test_db_reason_shapes_and_alias(self):
        cls, flag = turn_health.classify_db_reason(
            json.dumps({"reason": {"kind": "provider-failure",
                                   "class": "local-refusal"}}))
        self.assertEqual((cls, flag), ("provider-failure/local-refusal", False))
        cls, flag = turn_health.classify_db_reason(
            json.dumps({"reason": {"kind": "context-failure",
                                   "failure": "compaction-failed"}}))
        self.assertEqual((cls, flag), ("context-failure/compaction-failed", False))
        cls, flag = turn_health.classify_db_reason(
            json.dumps({"reason": {"kind": "policy-limit",
                                   "policy": "secode-runtime/step-policy/x/2"}}))
        self.assertEqual((cls, flag), ("policy-limit/secode-runtime/step-policy/x/2", False))
        cls, flag = turn_health.classify_db_reason(
            json.dumps({"reason": {"kind": "interrupted",
                                   "by": "managed-shutdown"}}))
        self.assertEqual((cls, flag), ("interrupted/managed-shutdown", True))
        cls, flag = turn_health.classify_db_reason(
            json.dumps({"reason": {"effect": {"kind": "ambiguous"},
                                   "kind": "process-loss"}}))
        self.assertEqual((cls, flag), ("process-loss/ambiguous", False))
        self.assertEqual(turn_health.classify_db_reason("not json"), (None, False))
        self.assertEqual(turn_health.classify_db_reason("{}"), (None, False))


class VerdictTests(unittest.TestCase):
    def test_execution_family_rows_outside_the_window_do_not_change_health(self):
        for outside in (WINDOW_START_MS - 1, UNTIL_MS + 1):
            with self.subTest(outside=outside), tempfile.TemporaryDirectory() as tmp:
                seat = make_seat(Path(tmp))
                t = WINDOW_START_MS
                add_events(seat, [admitted(t + 100, input_id="msg_done"),
                                  promoted(t + 110, input_id="msg_done"),
                                  started(t + 150), succeeded(t + 200),
                                  ("session.execution", outside, "ses_aaa111",
                                   json.dumps({"executionID": "exe_outside"}))])
                write_log(seat, "server.log", [dispatch_line(t + 400)])
                code, out = run_main(seat)
                self.assertEqual(code, 0, out)

    def test_execution_parser_requires_an_exact_known_type(self):
        for ending in ("session.execution.succeeded.1\n",):
            with self.subTest(ending=ending[:80]), tempfile.TemporaryDirectory() as tmp:
                seat = make_seat(Path(tmp))
                t = WINDOW_START_MS
                add_events(seat, [admitted(t + 100, input_id="msg_done"),
                                  promoted(t + 110, input_id="msg_done"),
                                  started(t + 150), succeeded(t + 200),
                                  (ending, t + 300, "ses_aaa111",
                                   json.dumps({"executionID": "exe_unknown"}))])
                write_log(seat, "server.log", [dispatch_line(t + 400)])
                code, out = run_main(seat)
                self.assertNotEqual(code, 0, out)

    def test_known_step_failure_is_not_also_reported_as_an_unknown_type(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            t = WINDOW_START_MS
            add_events(seat, [admitted(t + 100, input_id="msg_done"),
                              promoted(t + 110, input_id="msg_done"),
                              started(t + 150), succeeded(t + 200),
                              db_step_failed(t + 170, "ses_aaa111",
                                             {"kind": "context-failure",
                                              "failure": "compaction-failed"})])
            write_log(seat, "server.log", [dispatch_line(t + 400)])
            code, out = run_main(seat)
            self.assertEqual(code, 0, out)
            self.assertIn("context-failure/compaction-failed", out)
            self.assertNotIn("unrecognized/session.step.failed.3", out)

    def test_promotion_without_an_execution_outcome_cannot_claim_health(self):
        for earlier_success in (False, True):
            with self.subTest(earlier_success=earlier_success), \
                    tempfile.TemporaryDirectory() as tmp:
                seat = make_seat(Path(tmp))
                t = WINDOW_START_MS
                events = []
                if earlier_success:
                    events.extend([admitted(t + 100, input_id="msg_done"),
                                   promoted(t + 110, input_id="msg_done"),
                                   started(t + 150), succeeded(t + 200)])
                events.extend([admitted(t + 300, input_id="msg_pending"),
                               promoted(t + 310, input_id="msg_pending")])
                add_events(seat, events)
                write_log(seat, "server.log", [dispatch_line(t + 500)])
                code, out = run_main(seat)
                self.assertNotEqual(code, 0, out)

    def test_lost_terminal_names_its_session_when_the_start_precedes_the_window(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            t = WINDOW_START_MS
            sid = "ses_cross_window"
            add_events(seat, [admitted(t - 300, sid, input_id="msg_earlier"),
                              promoted(t - 290, sid, input_id="msg_earlier"),
                              started(t - 200, sid, "exe_earlier"),
                              db_interrupted(t + 100, sid, "shutdown", "exe_earlier")])
            write_log(seat, "server.log", [dispatch_line(t + 200)])
            code, out = run_main(seat)
            self.assertEqual(code, 1, out)
            self.assertIn(sid, out, out)

    def test_unrecognized_execution_spellings_cannot_hide_beside_a_success(self):
        for ending in ("session.execution.failed", "session.execution.expired",
                       "session.execution.failed.01"):
            with self.subTest(ending=ending), tempfile.TemporaryDirectory() as tmp:
                seat = make_seat(Path(tmp))
                t = WINDOW_START_MS
                add_events(seat, [admitted(t + 100, input_id="msg_done"),
                                  promoted(t + 110, input_id="msg_done"),
                                  started(t + 150), succeeded(t + 200),
                                  (ending, t + 300, "ses_aaa111",
                                   json.dumps({"executionID": "exe_unknown"}))])
                write_log(seat, "server.log", [dispatch_line(t + 400)])
                code, out = run_main(seat)
                self.assertNotEqual(code, 0, out)

    def test_unknown_execution_endings_cannot_be_reported_healthy(self):
        for ending in ("session.execution.failed.next",
                       "session.execution.expired.1", "future-interruption"):
            with self.subTest(ending=ending), tempfile.TemporaryDirectory() as tmp:
                seat = make_seat(Path(tmp))
                t = WINDOW_START_MS
                events = [admitted(t + 100), started(t + 150),
                          succeeded(t + 200)]
                if ending == "future-interruption":
                    events.extend([started(t + 250, execution="exe_other"),
                                   db_interrupted(t + 300, "ses_aaa111",
                                                  "future-reason", "exe_other")])
                else:
                    events.append((ending, t + 300, "ses_aaa111",
                                   json.dumps({"executionID": "exe_other"})))
                add_events(seat, events)
                write_log(seat, "server.log", [dispatch_line(t + 400)])
                code, out = run_main(seat)
                self.assertNotEqual(code, 0, out)

    def test_missing_execution_identity_cannot_be_paired_as_success(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            t = WINDOW_START_MS
            add_events(seat, [
                admitted(t + 100),
                ("session.execution.started.1", t + 150, "ses_aaa111", "{}"),
                ("session.execution.succeeded.1", t + 200, "ses_aaa111", "{}"),
            ])
            write_log(seat, "server.log", [dispatch_line(t + 300)])
            code, out = run_main(seat)
            self.assertNotEqual(code, 0, out)

    def test_promoted_steer_is_not_reported_as_never_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            t = WINDOW_START_MS
            events = []
            for offset, iid, delivery in ((100, "msg_initial", "queue"),
                                          (200, "msg_steer", "steer")):
                events.extend([
                    ("session.input.admitted.1", t + offset, "ses_aaa111",
                     json.dumps({"sessionID": "ses_aaa111", "inputID": iid,
                                 "input": {"type": "user", "delivery": delivery,
                                           "data": {"text": "continue"}}})),
                    ("session.input.promoted.1", t + offset + 10, "ses_aaa111",
                     json.dumps({"sessionID": "ses_aaa111", "inputID": iid})),
                ])
            events.extend([started(t + 150), succeeded(t + 400)])
            add_events(seat, events)
            write_log(seat, "server.log", [dispatch_line(t + 500)])
            code, out = run_main(seat)
            self.assertEqual(code, 0, out)

    def test_repeated_restart_losses_identify_the_broken_session(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            t = WINDOW_START_MS
            sid = "ses_restart_only"
            events = [admitted(t + 100, sid)]
            for i in range(3):
                eid = "exe_restart_{}".format(i)
                events.extend([started(t + 200 + 200 * i, sid, eid),
                               db_interrupted(t + 300 + 200 * i, sid,
                                              "shutdown", eid)])
            add_events(seat, events)
            write_log(seat, "server.log", [dispatch_line(t + 1000)])
            code, out = run_main(seat)
            self.assertEqual(code, 1, out)
            self.assertIn(sid, out.split("suspected brick)", 1)[1], out)

    def test_payload_budget_charges_utf8_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            payload = json.dumps({"executionID": "exe_other",
                                  "error": {"type": "ExecutionFailed",
                                            "message": "\U0001f642" * 96}},
                                 ensure_ascii=False)
            self.assertLess(len(payload), len(payload.encode("utf-8")))
            add_events(seat, [("session.execution.failed.1",
                               WINDOW_START_MS + 100, "ses_aaa111", payload)])
            with mock.patch.object(turn_health, "MAX_PAYLOAD_BYTES", len(payload)):
                result = turn_health.read_db(seat / "database.sqlite3",
                                             WINDOW_START_MS, UNTIL_MS)
            self.assertTrue(result.get("truncated") or not result["ok"], result)

    def test_one_success_cannot_cover_another_started_execution(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            t = WINDOW_START_MS
            add_events(seat, [admitted(t + 100), started(t + 150),
                              succeeded(t + 200), admitted(t + 250),
                              started(t + 300, execution="exe_pending")])
            write_log(seat, "server.log", [dispatch_line(t + 400)])
            code, out = run_main(seat)
            self.assertEqual(code, 2, out)

    def test_healthy_window_exits_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [
                dispatch_line(WINDOW_START_MS + 1000),
                "all quiet",
            ])
            add_events(seat, [
                admitted(WINDOW_START_MS + 2000, input_id="msg_1"),
                promoted(WINDOW_START_MS + 2010, input_id="msg_1"),
                started(WINDOW_START_MS + 2100),
                succeeded(WINDOW_START_MS + 4000),
                admitted(WINDOW_START_MS + 3000, input_id="msg_2"),
                promoted(WINDOW_START_MS + 3010, input_id="msg_2"),
                started(WINDOW_START_MS + 3100, execution="exe_2"),
                succeeded(WINDOW_START_MS + 4100, execution="exe_2"),
            ])
            code, out = run_main(seat)
        self.assertEqual(code, 0)
        self.assertIn("verdict  HEALTHY  (exit 0)", out)
        self.assertIn("started 2   succeeded 2   failed 0   lost 0   "
                      "interrupted 0", out)

    def test_failures_exit_one_with_classes_and_brick(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [
                dispatch_line(WINDOW_START_MS + 1000),
                step_failed("ses_brick1",
                            "ProviderFailure { class: LocalRefusal }",
                            "the attempt plan was refused: read capacity is "
                            "busy; retry the request"),
            ])
            add_events(seat, [
                admitted(WINDOW_START_MS + 100, "ses_brick1",
                         input_id="msg_1"),
                promoted(WINDOW_START_MS + 110, "ses_brick1",
                         input_id="msg_1"),
                started(WINDOW_START_MS + 150, "ses_brick1", "exe_a"),
                started(WINDOW_START_MS + 350, "ses_brick1", "exe_b"),
                started(WINDOW_START_MS + 450, "ses_brick1", "exe_c"),
                started(WINDOW_START_MS + 550, "ses_brick1", "exe_d"),
                exec_failed(WINDOW_START_MS + 1000, "ses_brick1", "exe_a",
                            "read capacity is busy"),
                exec_failed(WINDOW_START_MS + 2000, "ses_brick1", "exe_b",
                            "read capacity is busy"),
                exec_failed(WINDOW_START_MS + 3000, "ses_brick1", "exe_c",
                            "read capacity is busy"),
                db_interrupted(WINDOW_START_MS + 4000, "ses_brick1",
                               "shutdown", "exe_d"),
                db_step_failed(WINDOW_START_MS + 1000, "ses_brick1",
                               {"kind": "provider-failure",
                                "class": "local-refusal"}),
                db_step_failed(WINDOW_START_MS + 2000, "ses_brick1",
                               {"kind": "provider-failure",
                                "class": "local-refusal"}),
                db_step_failed(WINDOW_START_MS + 3000, "ses_brick1",
                               {"kind": "provider-failure",
                                "class": "local-refusal"}),
            ])
            code, out = run_main(seat)
        self.assertEqual(code, 1)
        self.assertIn(
            "verdict  FAILURES  (exit 1): 3 failed execution(s), "
            "1 lost to restart or process loss", out)
        self.assertIn("execution-failed/read-capacity-is-busy  0 | 3", out)
        self.assertIn("interrupted-by-restart/managed-shutdown  0 | 1", out)
        self.assertIn("provider-failure/local-refusal  0 | 3", out)
        self.assertIn("interrupted (not counted as failures)", out)
        self.assertIn("ses_brick1  3  last 2026-01-01T18:00:03Z", out)
        self.assertIn("failure rate (failed or lost, /started): 100.0%", out)

    def test_interrupted_alone_is_not_a_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [
                dispatch_line(WINDOW_START_MS + 1000),
                step_failed("ses_aaa111", "Interrupted { by: User }",
                            "attempt interrupted by the user"),
            ])
            add_events(seat, [
                admitted(WINDOW_START_MS + 100, input_id="msg_1"),
                promoted(WINDOW_START_MS + 110, input_id="msg_1"),
                started(WINDOW_START_MS + 150),
                db_interrupted(WINDOW_START_MS + 1000, "ses_aaa111", "user"),
            ])
            code, out = run_main(seat)
        self.assertEqual(code, 0)
        self.assertIn("interrupted/user  1 | 1", out)
        self.assertIn("verdict  HEALTHY  (exit 0)", out)

    def test_lost_work_interrupt_counts_but_user_does_not(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
            add_events(seat, [
                admitted(WINDOW_START_MS + 100, input_id="msg_1"),
                promoted(WINDOW_START_MS + 110, input_id="msg_1"),
                started(WINDOW_START_MS + 150),
                db_interrupted(WINDOW_START_MS + 1000, "ses_aaa111", "shutdown"),
                db_interrupted(WINDOW_START_MS + 2000, "ses_aaa111", "user"),
            ])
            code, out = run_main(seat)
        self.assertEqual(code, 1)
        self.assertIn(
            "verdict  FAILURES  (exit 1): 1 lost to restart or "
            "process loss", out)
        self.assertIn("interrupted-by-restart/managed-shutdown  0 | 1", out)
        self.assertIn("interrupted/user  0 | 1", out)

    def test_step_mirror_of_an_interrupted_execution_is_detail(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
            add_events(seat, [
                admitted(WINDOW_START_MS + 100, input_id="msg_1"),
                promoted(WINDOW_START_MS + 110, input_id="msg_1"),
                started(WINDOW_START_MS + 150),
                db_step_failed(WINDOW_START_MS + 1000, "ses_aaa111",
                               {"kind": "interrupted", "by": "user"}),
                db_interrupted(WINDOW_START_MS + 1000, "ses_aaa111", "user"),
            ])
            code, output = run_main(seat)
        self.assertEqual(code, 0, output)
        self.assertIn("interrupted/user  0 | 1", output)
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
            add_events(seat, [
                admitted(WINDOW_START_MS + 100, input_id="msg_1"),
                promoted(WINDOW_START_MS + 110, input_id="msg_1"),
                started(WINDOW_START_MS + 150),
                db_step_failed(WINDOW_START_MS + 1000, "ses_aaa111",
                               {"kind": "interrupted", "by": "user"}),
            ])
            code, output = run_main(seat)
        self.assertEqual(code, 2, output)
        self.assertIn("step-failure detail", output)
        self.assertIn("interrupted/user  0 | 1", output)

    def test_process_loss_detail_with_stranded_execution(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log",
                      [dispatch_line(WINDOW_START_MS + 120)])
            add_events(seat, [
                admitted(WINDOW_START_MS + 100, input_id="msg_1"),
                promoted(WINDOW_START_MS + 110, input_id="msg_1"),
                started(WINDOW_START_MS + 150),
            ])
            con = sqlite3.connect(str(seat / "database.sqlite3"))
            con.execute(
                "INSERT INTO event (id, aggregate_family, aggregate_id, seq, "
                "created, type, data) VALUES ('ev_0', 'session', 'ses_aaa111', "
                "1, ?, 'session.step.failed.2', ?)",
                (WINDOW_START_MS + 1000,
                 json.dumps({"reason": {"effect": {"kind": "ambiguous"},
                                         "kind": "process-loss"},
                             "step": 7})))
            con.commit()
            con.close()
            out = io.StringIO()
            with mock.patch.object(turn_health, "find_service",
                                   return_value=(4242, "/fake/secode-server",
                                                 None)), \
                    contextlib.redirect_stdout(out):
                code = turn_health.main(["--seat", str(seat), "--until", UNTIL])
        self.assertEqual(code, 1)
        self.assertIn("interrupted-by-restart/process-loss  0 | 1",
                      out.getvalue())

    def test_empty_window_is_unknown(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(1000)])
            add_events(seat, [admitted(1000)])
            code, out = run_main(seat)
        self.assertEqual(code, 2)
        self.assertIn("no log lines fall in the window", out)
        self.assertIn("no executions started and no inputs admitted in "
                      "the window", out)

    def test_absent_database_is_unknown_not_healthy(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
            code, out = run_main(seat)
        self.assertEqual(code, 2)
        self.assertIn("the session database is absent", out)

    def test_unreadable_database_is_unknown(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
            (seat / "database.sqlite3").write_bytes(b"not a sqlite file")
            code, out = run_main(seat)
        self.assertEqual(code, 2)
        self.assertIn("the session database is", out)

    def test_service_down_is_a_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
            out = io.StringIO()
            with mock.patch.object(
                    turn_health, "find_service",
                    return_value=(None, None,
                                  "no running product server names this seat")), \
                    contextlib.redirect_stdout(out):
                code = turn_health.main(["--seat", str(seat), "--until", UNTIL])
        self.assertEqual(code, 1)
        self.assertIn("no running product server names this seat", out.getvalue())

    def test_execution_failure_without_step_failure_is_not_healthy(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
            event = ("session.execution.failed.1", WINDOW_START_MS + 2000,
                     "ses_aaa111", json.dumps({"executionID": "exe_1",
                     "error": {"type": "ExecutionFailed", "message": "failed"}}))
            add_events(seat, [admitted(WINDOW_START_MS + 100,
                                        input_id="msg_1"),
                              promoted(WINDOW_START_MS + 110,
                                       input_id="msg_1"), event])
            code, output = run_main(seat)
        self.assertEqual(code, 1, output)

    def test_v2_failures_keep_their_failure_and_restart_classes(self):
        for reason in [{"kind": "provider-failure", "class": "local-refusal"},
                       {"kind": "interrupted", "by": "managed-shutdown"}]:
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as tmp:
                seat = make_seat(Path(tmp))
                write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
                event = ("session.step.failed.2", WINDOW_START_MS + 2000,
                         "ses_aaa111", json.dumps({"step": 7, "reason": reason}))
                add_events(seat, [admitted(WINDOW_START_MS + 100,
                                            input_id="msg_1"), event])
                code, output = run_main(seat)
                self.assertEqual(code, 1, output)

    def test_log_only_failure_cannot_be_hidden_by_a_readable_database(self):
        for reason in ["ProviderFailure { class: LocalRefusal }",
                       "Interrupted { by: ManagedShutdown }"]:
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as tmp:
                seat = make_seat(Path(tmp))
                write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000),
                    step_failed("ses_aaa111", reason, "attempt failed")])
                add_events(seat, [admitted(WINDOW_START_MS + 100,
                                            input_id="msg_1")])
                code, output = run_main(seat)
                self.assertNotEqual(code, 0, output)

    def test_uri_reserved_characters_do_not_redirect_or_create_a_database(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            seat = root / "seat#fragment"
            seat.mkdir()
            add_events(seat, [admitted(WINDOW_START_MS + 100)])
            before = sorted(path.name for path in root.iterdir())
            result = turn_health.read_db(seat / "database.sqlite3",
                                         WINDOW_START_MS, UNTIL_MS)
            after = sorted(path.name for path in root.iterdir())
        self.assertEqual(after, before, "read-only measurement created a file")
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["counts"]["admitted"], 1)

    def test_hosts_without_nice_still_produce_a_report(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
            add_events(seat, [admitted(WINDOW_START_MS + 100,
                                        input_id="msg_1"),
                              promoted(WINDOW_START_MS + 110,
                                       input_id="msg_1"),
                              started(WINDOW_START_MS + 150),
                              succeeded(WINDOW_START_MS + 4000)])
            with mock.patch.dict(turn_health.os.__dict__):
                del turn_health.os.nice
                code, output = run_main(seat)
        self.assertEqual(code, 0, output)

    def test_admission_without_a_terminal_is_unknown(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
            add_events(seat, [admitted(WINDOW_START_MS + 100,
                                        input_id="msg_1"),
                              promoted(WINDOW_START_MS + 110,
                                       input_id="msg_1"),
                              started(WINDOW_START_MS + 150)])
            code, output = run_main(seat)
        self.assertEqual(code, 2, output)

    def test_unknown_failure_versions_cannot_hide_behind_a_success(self):
        for kind in ["session.step.failed.4", "session.execution.failed.2"]:
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                seat = make_seat(Path(tmp))
                write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 1000)])
                event = (kind, WINDOW_START_MS + 2000, "ses_aaa111", "{}")
                add_events(seat, [admitted(WINDOW_START_MS + 100),
                                 succeeded(WINDOW_START_MS + 200), event])
                code, output = run_main(seat)
                self.assertNotEqual(code, 0, output)

    def test_one_success_cannot_cover_a_second_unfinished_turn(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 3000)])
            add_events(seat, [admitted(WINDOW_START_MS + 100,
                                        input_id="msg_1"),
                              promoted(WINDOW_START_MS + 110,
                                       input_id="msg_1"),
                              succeeded(WINDOW_START_MS + 200),
                              admitted(WINDOW_START_MS + 2000,
                                       input_id="msg_2")])
            code, output = run_main(seat)
        self.assertNotEqual(code, 0, output)

    def test_an_unrelated_user_interrupt_cannot_hide_later_lost_work(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            write_log(seat, "server.log", [dispatch_line(WINDOW_START_MS + 3000)])
            add_events(seat, [
                admitted(WINDOW_START_MS + 100, input_id="msg_1"),
                db_interrupted(WINDOW_START_MS + 200, "ses_aaa111", "user"),
                db_step_failed(WINDOW_START_MS + 2000, "ses_aaa111",
                               {"kind": "interrupted", "by": "managed-shutdown"}),
            ])
            code, output = run_main(seat)
        self.assertEqual(code, 1, output)
        self.assertIn("interrupted-by-restart/managed-shutdown  0 | 1", output)

    def test_interruption_payloads_obey_the_same_byte_budget(self):
        with tempfile.TemporaryDirectory() as tmp:
            seat = make_seat(Path(tmp))
            event = db_interrupted(WINDOW_START_MS + 200, "ses_aaa111", "user")
            value = json.loads(event[3])
            value['padding'] = 'x' * 4096
            add_events(seat, [admitted(WINDOW_START_MS + 100),
                              (*event[:3], json.dumps(value))])
            with mock.patch.object(turn_health, 'MAX_PAYLOAD_BYTES', 128):
                state = turn_health.read_db(seat/'database.sqlite3', WINDOW_START_MS, UNTIL_MS)
        self.assertTrue(state.get('truncated') or not state['ok'], state)


class BuildCheckTests(unittest.TestCase):
    def make_repo(self, tmp, ahead):
        repo = tmp / "repo"
        repo.mkdir()
        env = dict(os.environ,
                   GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
                   GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid")

        def gitc(*args):
            subprocess.run(["git", "-C", str(repo), *args], check=True, env=env,
                           capture_output=True)

        def head():
            return subprocess.run(["git", "-C", str(repo), "rev-parse", "HEAD"],
                                  check=True, env=env,
                                  capture_output=True, text=True).stdout.strip()

        gitc("init", "-q", "-b", "main")
        (repo / "f").write_text("1\n")
        gitc("add", "f")
        gitc("commit", "-qm", "one")
        first = head()
        for i in range(2, ahead + 2):
            (repo / "f").write_text("{}\n".format(i))
            gitc("add", "f")
            gitc("commit", "-qm", "commit {}".format(i))
        gitc("update-ref", "refs/remotes/github/main", head())
        return repo, first, head()

    def make_binary(self, tmp, attested):
        package = tmp / "package"
        package.mkdir()
        exe = package / "secode-server"
        exe.write_bytes(b"binary")
        if attested is not None:
            manifest = {"engine_version": attested,
                        "binaries": {exe.name: {
                            "sha256": hashlib.sha256(b"binary").hexdigest(),
                            "source_commit": attested}}}
            (package / "release-manifest.json").write_text(
                json.dumps(manifest), encoding="utf-8")
        return exe

    def test_behind_main(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            repo, first, head = self.make_repo(tmp, 1)
            exe = self.make_binary(tmp, first)
            lines = turn_health.build_check(exe, repo)
        self.assertIn("  attested build: {}".format(first), lines)
        self.assertIn("  github/main: 1 commit(s) behind main ({})".format(head[:12]),
                      lines)

    def test_up_to_date(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            repo, first, head = self.make_repo(tmp, 1)
            exe = self.make_binary(tmp, head)
            lines = turn_health.build_check(exe, repo)
        self.assertIn("  github/main: up to date with main", lines)

    def test_unattested(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            repo, first, head = self.make_repo(tmp, 1)
            exe = self.make_binary(tmp, None)
            lines = turn_health.build_check(exe, repo)
        self.assertEqual(lines, ["  attested build: none (unattested binary)"])

    def test_commit_absent_from_clone(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            repo, first, head = self.make_repo(tmp, 1)
            exe = self.make_binary(tmp, "0" * 40)
            lines = turn_health.build_check(exe, repo)
        self.assertIn("attested commit is not in this clone", lines[-1])

    def test_no_main_ref_reachable(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            repo, first, head = self.make_repo(tmp, 1)
            subprocess.run(["git", "-C", str(repo), "update-ref", "-d",
                            "refs/remotes/github/main"], check=True)
            exe = self.make_binary(tmp, first)
            lines = turn_health.build_check(exe, repo)
        self.assertIn("no main ref reachable in the given repo", lines[-1])

    def test_stale_remote_tracking_ref_cannot_claim_current_main(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            repo, first, head = self.make_repo(tmp, 1)
            remote = tmp / "upstream.git"
            subprocess.run(["git", "clone", "--bare", str(repo), str(remote)],
                           check=True, capture_output=True)
            subprocess.run(["git", "-C", str(repo), "remote", "add", "github",
                            str(remote)], check=True, capture_output=True)
            subprocess.run(["git", "-C", str(repo), "update-ref",
                            "refs/remotes/github/main", first], check=True)
            exe = self.make_binary(tmp, first)
            lines = turn_health.build_check(exe, repo)
        self.assertFalse(any("up to date with main" in line for line in lines), lines)

    def test_mismatched_binary_cannot_claim_an_attested_current_build(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            repo, first, head = self.make_repo(tmp, 1)
            exe = self.make_binary(tmp, head)
            manifest = exe.parent / "release-manifest.json"
            manifest.write_text(json.dumps({"engine_version": head, "binaries": {
                exe.name: {"sha256": "0" * 64, "source_commit": head}}}))
            lines = turn_health.build_check(exe, repo)
        self.assertFalse(any("up to date with main" in line for line in lines), lines)

    def test_missing_digest_entries_cannot_claim_attested_builds(self):
        for binaries in [None, {}, {"different-server": {"sha256": "0" * 64}}]:
            with self.subTest(binaries=binaries), tempfile.TemporaryDirectory() as tmp:
                tmp = Path(tmp)
                repo, first, head = self.make_repo(tmp, 1)
                exe = self.make_binary(tmp, head)
                value = {"engine_version": head}
                if binaries is not None:
                    value["binaries"] = binaries
                (exe.parent / "release-manifest.json").write_text(json.dumps(value))
                lines = turn_health.build_check(exe, repo)
                self.assertFalse(any("up to date with main" in line for line in lines), lines)


class ArgumentTests(unittest.TestCase):
    def test_seat_required(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(SystemExit):
                turn_health.parse_args([])

    def test_parse_when_forms(self):
        self.assertEqual(turn_health.parse_when(UNTIL), UNTIL_MS / 1000.0)
        self.assertEqual(turn_health.parse_when(str(UNTIL_MS)), UNTIL_MS / 1000.0)
        self.assertEqual(turn_health.parse_when(str(int(UNTIL_MS / 1000))),
                         UNTIL_MS / 1000.0)


if __name__ == "__main__":
    unittest.main()
