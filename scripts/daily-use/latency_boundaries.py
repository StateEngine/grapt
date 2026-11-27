"""Join observed dispatch times to durable admission and output by exact identity.

Reads an isolated database and optional product stderr. Missing diagnostics stay
null. The durable send permit is deliberately not used as a dispatch timestamp.
Submission-to-admission needs a separately observed client submission; these
server-side inputs cannot provide it. Output times mean durable append, not paint.
"""
import argparse
import hashlib
import json
import re
import sqlite3
from pathlib import Path

LIMIT = 131072
DISPATCH = re.compile(r"GRAPT_PROVIDER_DISPATCH attempt=([A-Za-z0-9_-]{1,512}) at_ms=([0-9]{1,19})")
LOSS = re.compile(r"GRAPT_PROVIDER_DISPATCH_LOSS count_at_least=([0-9]{1,20})")


def dispatches(path):
    stamps, lost = {}, 0
    if path is None:
        return stamps, None
    with path.open("rb") as source:
        raw = source.read(16 * 1024 * 1024 + 1)
    if len(raw) > 16 * 1024 * 1024:
        raise ValueError("diagnostic log exceeds 16 MiB; use a bounded run's log")
    for line in raw.decode("utf-8").splitlines():
        if not line.startswith("GRAPT_PROVIDER_DISPATCH"):
            continue
        sample, loss = DISPATCH.fullmatch(line), LOSS.fullmatch(line)
        if loss:
            lost = max(lost, int(loss[1]))
        elif sample:
            attempt, stamp = sample[1], int(sample[2])
            if attempt in stamps or stamp > 2**63 - 1 or len(stamps) >= LIMIT:
                raise ValueError("duplicate, invalid or excessive dispatch observations")
            stamps[attempt] = stamp
        else:
            raise ValueError("malformed dispatch diagnostic")
    return stamps, lost


def interval(start, end):
    if start is None or end is None:
        return None
    if end < start:
        raise ValueError("observed clock moved backwards; interval cannot be compared")
    return end - start


def submission_windows(path, client, explicit_session):
    if path is None:
        return [], []
    with path.open("rb") as source:
        raw = source.read(16 * 1024 * 1024 + 1)
    if len(raw) > 16 * 1024 * 1024:
        raise ValueError("comparison exceeds 16 MiB")
    sides = [side for side in json.loads(raw)["sides"] if side["client"] == client]
    if len(sides) != 1 or sides[0]["session_binding_error"]:
        raise ValueError("comparison has no unambiguous client/session binding")
    side = sides[0]
    windows = [step for step in side["steps"] if step.get("submission") is True]
    sessions = [explicit_session] if explicit_session else side["sessions_created"]
    if windows and not sessions:
        raise ValueError("reopened session needs its explicit observed --session identity")
    for index, window in enumerate(windows):
        if window["kind"] != "key" or window["detail"] != "Enter":
            raise ValueError("submission marker does not name a delivered Enter key")
        start, end = window["action_started_ms"], window["action_completed_ms"]
        if type(start) is not int or type(end) is not int or end < start:
            raise ValueError("invalid submission observation window")
        window["admission_before_ms"] = (windows[index+1]["action_started_ms"]
                                         if index+1 < len(windows)
                                         else side["steps"][-1]["action_completed_ms"] + 1)
    return sessions, windows


def report(database, stderr=None, comparison=None, client="retained", session=None):
    explicit_session = session
    stamps, lost = dispatches(stderr)
    admissions, promoted, messages, samples = {}, {}, {}, []
    kinds = ("session.input.admitted.1", "session.input.promoted.1",
             "session.assistant-message.opened.2", "session.content.appended.1")
    connection = sqlite3.connect(database.resolve().as_uri() + "?mode=ro", uri=True)
    try:
        rows = connection.execute("""SELECT aggregate_id,type,created,
            CASE WHEN length(CAST(data AS BLOB)) <= 262144 THEN data END
            FROM event WHERE aggregate_family='session' AND type IN (?,?,?,?)
            ORDER BY aggregate_id,seq LIMIT ?""", (*kinds, LIMIT + 1))
        for count, (session, kind, created, raw) in enumerate(rows, 1):
            if count > LIMIT or raw is None:
                raise ValueError("event evidence exceeds its row or payload bound")
            data = json.loads(raw)
            if kind == kinds[0]:
                text = data["input"]["data"].get("text")
                admissions[(session, data["inputID"])] = {
                    "admitted_ms": created,
                    "prompt_sha256": hashlib.sha256(text.encode()).hexdigest() if isinstance(text, str) else None,
                }
            elif kind == kinds[1]:
                key = (session, data["inputID"])
                if key not in admissions:
                    raise ValueError("promotion has no corresponding admission")
                promoted[session] = (data["inputID"], admissions[key])
            elif kind == kinds[2]:
                input_id, admission = promoted.get(session, (None, {}))
                sample = dict(session=session, input_id=input_id, attempt=data["attempt"],
                              step=data["step"], selection=data["selection"],
                              submitted_ms=None, admitted_ms=admission.get("admitted_ms"),
                              prompt_sha256=admission.get("prompt_sha256"),
                              assistant_opened_ms=created, dispatch_ms=stamps.get(data["attempt"]),
                              first_content_ms=None, first_text_ms=None)
                key = (session, data["message"])
                if key in messages:
                    raise ValueError("assistant message opened twice")
                messages[key] = sample
                samples.append(sample)
            elif (session, data["message"]) in messages:
                sample = messages[(session, data["message"])]
                if sample["first_content_ms"] is None:
                    sample["first_content_ms"] = created
                if data["item"]["kind"] == "text" and sample["first_text_ms"] is None:
                    sample["first_text_ms"] = created
    finally:
        connection.close()
    selected_sessions, windows = submission_windows(comparison, client, explicit_session)
    observed_submissions = {}
    for window in windows:
        candidates = [(key, value) for key, value in admissions.items()
                      if key[0] in selected_sessions
                      and window["action_started_ms"] <= value["admitted_ms"] < window["admission_before_ms"]]
        if len(candidates) != 1:
            raise ValueError("submission window does not bind exactly one durable admission")
        key, admission = candidates[0]
        if key in observed_submissions:
            raise ValueError("admission bound to two submission windows")
        observed_submissions[key] = window
    for sample in samples:
        window = observed_submissions.get((sample["session"], sample["input_id"]))
        if window:
            sample["submission_action_window_ms"] = [window["action_started_ms"], window["action_completed_ms"]]
            sample["submission_to_admission_bounds_ms"] = [
                max(0, sample["admitted_ms"] - window["action_completed_ms"]),
                sample["admitted_ms"] - window["action_started_ms"],
            ]
        for name, start, end in [
            ("submission_to_admission_ms", "submitted_ms", "admitted_ms"),
            ("admission_to_dispatch_ms", "admitted_ms", "dispatch_ms"),
            ("dispatch_to_first_content_ms", "dispatch_ms", "first_content_ms"),
            ("dispatch_to_first_text_ms", "dispatch_ms", "first_text_ms"),
        ]:
            sample[name] = interval(sample[start], sample[end])
    return {"schema": "daily-use-latency-boundaries.v1", "samples": samples,
            "dispatch_samples": len(stamps), "dispatch_lost_at_least": lost,
            "unmatched_dispatch_attempts": sorted(set(stamps) - {sample["attempt"] for sample in samples}),
            "limits": "Wall-clock observations; output is durable append, not frontend paint. Submission bounds, when present, bracket the external observer action rather than claiming a frontend key-handler timestamp. Later Steps include prior Steps and user waits in admission-to-dispatch."}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database", type=Path)
    parser.add_argument("--stderr", type=Path)
    parser.add_argument("--comparison", type=Path)
    parser.add_argument("--client", choices=["retained", "native"], default="retained")
    parser.add_argument("--session", help="Observed session identity for a reopen comparison")
    args = parser.parse_args()
    print(json.dumps(report(args.database, args.stderr, args.comparison, args.client, args.session), indent=2))
