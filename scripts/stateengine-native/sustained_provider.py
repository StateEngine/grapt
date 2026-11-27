"""Declared useful work; only the model is scripted, all effects use product tools."""
import json
import threading
import os
import subprocess
import sys

from acceptance_peer import Model

TASK = "SUSTAIN_TASK: fix signed addition in calc.py, run its tests and preserve the objective."
QUEUED = "SUSTAIN_QUEUED: after the current work, record that this queued input completed."
RESTART = "SUSTAIN_RESTART: keep the verified addition, add subtraction and run all tests."
SUMMARY = """## Objective
- SUSTAIN_TASK: fix signed addition in calc.py, run its tests and preserve the objective.
## Important Details
- calc.add now returns a + b. SUSTAIN_TOOL_DONE: two unit tests passed with OK.
- Background terminal output was polled through its owning handle, not replayed.
## Work State
### Completed
- Signed addition fixed and verified; the declared evidence files were read.
### Active
- Complete the current objective, then consume SUSTAIN_QUEUED once.
### Blocked
- none
## Next Move
1. Finish this work and retain its result across the disposable service restart.
## Relevant Files
- calc.py, test_calc.py, worklog.txt and evidence-*.txt belong to this fixture.
"""


def test_command(background=False):
    if os.name == "nt":
        if background:
            code = ("import subprocess,sys,time; print('SUSTAIN_TOOL_BEGIN',flush=True); "
                "time.sleep(3); r=subprocess.run([sys.executable,'-m','unittest','-v']); "
                "print('SUSTAIN_TOOL_DONE',flush=True); sys.exit(r.returncode)")
            return subprocess.list2cmdline([sys.executable, "-c", code])
        return subprocess.list2cmdline([sys.executable, "-m", "unittest", "-v"])
    if background:
        return "printf 'SUSTAIN_TOOL_BEGIN\\n'; sleep 3; python3 -m unittest -v; test_rc=$?; printf 'SUSTAIN_TOOL_DONE\\n'; exit \"$test_rc\""
    return "python3 -m unittest -v"


def output(body, call):
    rows = [row for row in body.get("input", []) if row.get("call_id") == call
            and row.get("type") in ("function_call_output", "custom_tool_call_output")]
    if len(rows) != 1:
        raise ValueError(f"expected exactly one linked output for {call}")
    value = rows[0]["output"]
    return json.loads(value) if isinstance(value, str) else value


def frames(events):
    events = [*events, {"type": "response.completed", "response": {"usage": {
        "input_tokens": 30, "output_tokens": 10, "total_tokens": 40}}}]
    return "text/event-stream", "".join("data: " + json.dumps(x) + "\n\n" for x in events).encode()


def text(value):
    return frames([{"type": "response.output_text.delta", "delta": value}, {"type": "response.output_text.done"}])


def calls(items):
    events = []
    for call, name, args in items:
        custom = name == "apply_patch"
        events.extend([
            {"type": "response.output_item.added", "item": {"type": "custom_tool_call" if custom else "function_call",
                "id": call, "call_id": call, "name": name}},
            {"type": "response.custom_tool_call_input.delta" if custom else "response.function_call_arguments.delta",
                "item_id": call, "delta": args if custom else json.dumps(args)},
            {"type": "response.custom_tool_call_input.done" if custom else "response.function_call_arguments.done",
                "item_id": call, **({"input": args} if custom else {})}])
    return frames(events)


class SustainedModel(Model):
    def __init__(self):
        super().__init__()
        self.stage, self.summaries, self.polls = 0, 0, 0
        self.summary_entered, self.summary_release = threading.Event(), threading.Event()
        self.observed = []
        self.handles = []
        self.last_poll = None
        self.tool_output = ""

    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        encoded = json.dumps(body)
        if "Output exactly the Markdown structure shown inside <template>" in encoded:
            self.summaries += 1
            if self.summaries > 4 or "calc.py" not in encoded or "OK" not in encoded:
                raise ValueError("summary lost the objective or verified tool outcome")
            self.observed.append({"action": "summary", "ordinal": self.summaries})
            self.summary_entered.set()
            if not self.summary_release.wait(timeout=40):
                raise TimeoutError("summary was not released after queued input admission")
            return text(SUMMARY)
        self.observed.append({"action": "coding", "stage": self.stage})
        stage = self.stage
        if stage == 0:
            if TASK not in encoded:
                raise ValueError("original producer objective missing")
            reply = calls([("call_sustain_add", "apply_patch", "*** Begin Patch\n*** Update File: calc.py\n@@\n-    return abs(a) + abs(b)\n+    return a + b\n*** End Patch")])
        elif stage in (1, 8):
            if stage == 1 and "call_sustain_add" not in encoded:
                raise ValueError("patch outcome missing")
            self.tool_output = ""
            self.polls = 0
            self.last_poll = "call_sustain_exec_" + str(stage)
            reply = calls([(self.last_poll, "exec_command", {
                "cmd": test_command(background=True),
                "tty": True, "yield_time_ms": 250, "timeout_ms": 20000, "max_output_tokens": 1000})])
        elif stage in (2, 9):
            result = output(body, self.last_poll)
            self.tool_output += result.get("output", "")
            if self.polls == 0:
                if result.get("done") is not False or not result.get("session_id"):
                    raise ValueError("command did not return a real background terminal handle")
                self.handles.append(result["session_id"])
                if len(set(self.handles)) != len(self.handles):
                    raise ValueError("restart resurrected an earlier terminal identity")
            if not result.get("done"):
                self.polls += 1
                if self.polls > 8:
                    raise ValueError("background command did not settle within eight polls")
                self.last_poll = f"call_sustain_poll_{stage}_{self.polls}"
                return calls([(self.last_poll, "write_stdin", {"session_id": self.handles[-1],
                    "chars": "", "yield_time_ms": 1000, "max_output_tokens": 1000})])
            if result.get("exit_code") != 0 or "OK" not in self.tool_output or "SUSTAIN_TOOL_DONE" not in self.tool_output:
                raise ValueError("useful background tool tests failed or lost output")
            self.stage += 1
            return self.dispatch_continued(body)
        elif stage == 3:
            reply = calls([(f"call_sustain_read_{n}", "read", {"path": f"evidence-{n}.txt"}) for n in range(8)])
        elif stage == 4:
            if not self.summaries or "Model-written continuation summary" not in encoded or "SUSTAIN_TOOL_DONE" not in encoded:
                raise ValueError("automatic compaction lost the objective or completed tool outcome")
            reply = text("SUSTAIN_INITIAL_DONE")
        elif stage == 5:
            if QUEUED not in encoded:
                raise ValueError("the queued input was lost")
            reply = calls([("call_sustain_queued", "apply_patch", "*** Begin Patch\n*** Add File: worklog.txt\n+addition verified; queued input completed once\n*** End Patch")])
        elif stage == 6:
            reply = text("SUSTAIN_QUEUED_DONE")
        elif stage == 7:
            if RESTART not in encoded or "SUSTAIN_TOOL_DONE" not in encoded:
                raise ValueError("restart lost the continued objective or verified earlier tool outcome")
            reply = calls([("call_sustain_subtract", "apply_patch", "*** Begin Patch\n*** Update File: calc.py\n@@\n     return a + b\n+\n+def subtract(a, b):\n+    return a - b\n*** Update File: test_calc.py\n@@\n-    def test_zero(self): self.assertEqual(calc.add(0, 0), 0)\n+    def test_zero(self): self.assertEqual(calc.add(0, 0), 0)\n+    def test_subtract(self): self.assertEqual(calc.subtract(-2, 5), -7)\n*** End Patch")])
        elif stage == 10:
            reply = text("SUSTAIN_RESTART_DONE")
        else:
            raise ValueError("unexpected replay or extra provider request")
        self.stage += 1
        return reply

    def dispatch_continued(self, body):
        """Choose the next scripted response without recording a second physical request."""
        if self.stage == 3:
            self.stage = 4
            return calls([(f"call_sustain_read_{n}", "read", {"path": f"evidence-{n}.txt"}) for n in range(8)])
        if self.stage == 10:
            self.stage = 11
            return text("SUSTAIN_RESTART_DONE")
        raise ValueError("unexpected terminal continuation stage")
