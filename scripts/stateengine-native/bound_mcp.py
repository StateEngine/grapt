"""Real bound MCP journey; requires disposable loopback-provider-testkit binaries.

Supply an immutable StateEngine source snapshot with its test fixture helpers.
Only the model is scripted: issuance, CLI provision, MCP resume/list/send and
the independent committed-message readback use the real engine.
"""
import argparse
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
from types import SimpleNamespace

sys.dont_write_bytecode = True

import headless_acceptance as headless
from acceptance_peer import Model, Server
from real_seat_model import object_result
from sustained_provider import calls, output, text

SESSION = "ses_se0123456789abcdef01234567"
MARKER = "BOUND_REAL_STATEENGINE_SEND"


def reply(process):
    with selectors.DefaultSelector() as ready:
        ready.register(process.stdout, selectors.EVENT_READ)
        headless.require(bool(ready.select(75)), "real engine fixture reply timeout")
        raw = process.stdout.readline(65537)
        headless.require(raw and len(raw) <= 65536, "real engine fixture reply bound")
        return json.loads(raw)


def command(process, action, **values):
    process.stdin.write(json.dumps({"action": action, **values}) + "\n")
    process.stdin.flush()
    return reply(process)


class BoundTools(Model):
    def __init__(self, baseline):
        super().__init__()
        self.baseline = baseline
        self.checks = {}
        self.diagnostics = {}
        self.engine = self.binding = self.packet = None

    @staticmethod
    def execute(call, code):
        return calls([(call, "execute", {"code": code})])

    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        step = self.count()
        if step == 1:
            return self.execute("discover", "return {resume:catalog.search('stateengine_resume',16),"
                                "inbox:catalog.search('inbox',16),"
                                "sentence:catalog.search('stateengine resume inbox messages read send',8),"
                                "skill:catalog.search('skill',8),"
                                "list:catalog.describe('stateengine_stateengine_msg_list')};")
        if step == 2:
            found = output(body, "discover")
            self.checks["resume_discovered"] = any(row["name"] == "stateengine_stateengine_resume"
                                                   for row in found["resume"]["matches"])
            self.checks["list_discovered"] = found["list"]["name"] == "stateengine_stateengine_msg_list"
            self.checks["inbox_discovered"] = any(row["name"] == "stateengine_stateengine_msg_list"
                                                  for row in found["inbox"]["matches"])
            self.checks["sentence_discovered"] = all(any(row["name"] == "stateengine_stateengine_" + name
                for row in found["sentence"]["matches"]) for name in ["resume", "msg_list", "msg_send"])
            # Skills are a built-in tool, not a deferred MCP registration.
            self.checks["skill_catalog_honest"] = found["skill"]["total"] == 0
            if self.baseline:
                return self.execute("list", "return await tools.stateengine_stateengine_msg_list({limit:10});")
            headless.require(all(self.checks.values()), "real tools absent from CodeMode")
            return self.execute("resume", "return await tools.stateengine_stateengine_resume("
                                + json.dumps({"packet_id": self.packet}) + ");")
        if step == 3 and self.baseline:
            rows = [row for row in body.get("input", [])
                    if row.get("call_id") == "list" and row.get("type") == "function_call_output"]
            self.checks["placement_refused"] = len(rows) == 1 and "absolute" in rows[0]["output"]
            headless.require(self.checks["placement_refused"], "baseline failed for another reason")
            return text("BOUND_BASELINE_BROKEN")
        if step == 3:
            resumed = object_result(output(body, "resume"))
            principal = resumed["principal"]
            self.checks["resume_dispatched"] = (principal["project_id"] == self.binding["project"]
                and principal["agent_id"] == self.binding["agent"] and principal["verified"] is True)
            headless.require(self.checks["resume_dispatched"], "real resume principal mismatch")
            headless.require(command(self.engine, "seed-large-inbox")["seeded"], "inbox seed failed")
            return self.execute("large", "try {return {unexpected:await tools.stateengine_stateengine_msg_list("
                                "{limit:200,envelope:'full',full:true})};} catch(e) {return {failure:JSON.stringify(e)};}")
        if step == 4:
            large = output(body, "large")
            self.diagnostics["large_reply"] = {"bytes": len(json.dumps(large)),
                                               "failure": large.get("failure", "")[:500]}
            self.checks["oversize_isolated"] = "output exceeded 32768 bytes; use a smaller limit" in large.get("failure", "")
            headless.require(self.checks["oversize_isolated"], "large inbox did not produce the bounded refusal")
            return self.execute("list", "return await tools.stateengine_stateengine_msg_list({limit:10});")
        if step == 5:
            listed = object_result(output(body, "list"))
            self.checks["list_dispatched"] = isinstance(listed.get("messages"), list)
            headless.require(self.checks["list_dispatched"], "small read failed after oversized result")
            args = {"agent_id": self.binding["agent"], "to": self.binding["recipient"],
                    "kind": "handoff", "body": MARKER}
            return self.execute("send", "return await tools.stateengine_stateengine_msg_send("
                                + json.dumps(args) + ");")
        if step == 6:
            sent = object_result(output(body, "send"))
            self.checks["send_dispatched"] = (sent.get("append", {}).get("state") == "ok"
                and command(self.engine, "verify-send", msg_id=sent.get("msg_id"))["verified"])
            headless.require(self.checks["send_dispatched"], "real send was not committed exactly once")
            return text("BOUND_REAL_STATEENGINE_DONE")
        raise RuntimeError("unexpected provider call")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stateengine", type=Path, required=True)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    parser.add_argument("--node", default="node")
    parser.add_argument("--expect-broken", action="store_true")
    args = parser.parse_args()
    args.scratch.mkdir(mode=0o700, exist_ok=False)
    for name in ["home", "tmp"]:
        (args.scratch / name).mkdir()
    os.environ.clear()
    os.environ.update(PATH="/usr/local/bin:/usr/bin:/bin", HOME=str(args.scratch / "home"),
                      TMPDIR=str(args.scratch / "tmp"), LANG="C.UTF-8", PYTHONDONTWRITEBYTECODE="1")
    headless.SESSION = SESSION
    model = BoundTools(args.expect_broken)
    journey = engine = None
    result = {"passed": False, "real_stateengine": True, "fixture_packet": True,
              "paid_provider_calls": 0, "live_profile_touched": False}
    try:
        with Server(0, model.dispatch) as provider, (args.scratch / "engine.stderr").open("w") as log:
            inputs = SimpleNamespace(scratch=args.scratch, product=args.binaries / "grapt-server",
                                     cli=args.binaries / "grapt")
            journey = headless.Journey(inputs, "bound-real", provider.endpoint)
            profile = journey.profile / "grapt.jsonc"
            config = json.loads(profile.read_text())
            config.pop("mcp")  # This journey uses the real launcher-generated declaration.
            config["permission"]["execute"] = "allow"
            headless.write(profile, config)
            journey.start(0)
            engine = subprocess.Popen([args.node, str(Path(__file__).with_name("bound_mcp_prepare.mjs")),
                                       str(args.stateengine)], env=journey.environment,
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, text=True)
            model.engine, model.binding = engine, reply(engine)
            receipt = command(engine, "launch", cli=str(inputs.cli), service=str(journey.profile), session=SESSION)["receipt"]
            packet = Path(receipt["grapt_packet"]["path"]).read_text()
            model.packet = next(line.split(": ", 1)[1] for line in packet.splitlines() if line.startswith("packet_id: "))
            headless.require("GRAPT_CONFIG" not in journey.environment, "service inherited config selector")
            run = journey.run("real-bound-tools")
            result.update(cli_exit=run.returncode, checks=model.checks, diagnostics=model.diagnostics)
            if args.expect_broken:
                headless.require(not model.checks.get("resume_discovered") and model.checks.get("placement_refused"),
                                 "baseline did not demonstrate both regressions")
                result.update(passed=True, expected_broken=True)
            else:
                headless.require(run.returncode == 0 and len(model.checks) == 9 and all(model.checks.values()),
                                 "real bound tools did not complete")
                headless.require("BOUND_REAL_STATEENGINE_DONE" in json.dumps(journey.history()), "completion absent")
                result.update(passed=True, actual_cli_provision=True, service_without_selector=True)
    finally:
        if journey:
            journey.cleanup()
        if engine:
            if engine.poll() is None:
                engine.stdin.write('{"action":"exit"}\n')
                engine.stdin.flush()
                engine.stdin.close()
                try:
                    engine.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    engine.kill()
                    engine.wait(timeout=5)
            result["engine_exit"] = engine.returncode
        (args.scratch / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
