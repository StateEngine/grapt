"""Deterministic model recipe for packet, mail and compaction seat journeys."""
import importlib.util
import json
from pathlib import Path
import sys
import threading
import time

sys.dont_write_bytecode = True
path = Path(__file__).resolve().parents[1] / "native-tui/loopback-provider.py"
spec = importlib.util.spec_from_file_location("seat_provider_transport", path)
transport = importlib.util.module_from_spec(spec)
spec.loader.exec_module(transport)
observations = Path("provider-observations.jsonl")
guard = threading.Lock()
current = threading.local()
count = 0
LABELS = ("SEAT_LAUNCH", "SEAT_BUSY", "SEAT_COMPACTED", "msg_seat_idle", "msg_seat_busy")


def mail_result(rows, message):
    """A continuation must contain this request's exact linked full read."""
    calls = [row for row in rows if row.get("type") == "function_call"]
    outputs = [row for row in rows if row.get("type") == "function_call_output"]
    if not calls and not outputs:
        return None
    if len(calls) != 1 or len(outputs) != 1:
        raise ValueError("mail continuation requires exactly one read and result")
    call, output = calls[0], outputs[0]
    expected = {"msg_id": message, "body": "SEAT_MAIL_" + message.removeprefix("msg_seat_").upper()}
    value = json.loads(output["output"])
    if (call.get("name") != "stateengine_stateengine_msg_read"
            or json.loads(call["arguments"]) != {"msg_id": message}
            or not call.get("call_id") or call["call_id"] != output.get("call_id")
            or value.get("message") != expected
            or value.get("read_acknowledgement") != "recorded"):
        raise ValueError("mail result identity, body or call linkage mismatch")
    return expected


def observe(body):
    global count
    entered = time.monotonic_ns()
    with guard:
        count += 1
        if count > 32:
            raise ValueError("seat provider request bound")
        with observations.with_name("provider-attempts.jsonl").open("a") as output:
            output.write(json.dumps({"at_ns": entered}) + "\n")
    text = body.decode()
    data = json.loads(text)
    summary = "Output exactly the Markdown structure shown inside <template>" in text
    label = "summary" if summary else None
    current.tool_result = False
    mail = None
    if not summary:
        rows = data.get("input", [])
        for index in range(len(rows) - 1, -1, -1):
            row = rows[index]
            if row.get("role") == "user":
                rendered = json.dumps(row.get("content"))
                label = next((marker for marker in LABELS if marker in rendered), None)
                if label:
                    suffix = rows[index + 1:]
                    if label.startswith("msg_seat_"):
                        mail = mail_result(suffix, label)
                        current.tool_result = mail is not None
                    else:
                        current.tool_result = any(item.get("type") == "function_call_output"
                                                  for item in suffix)
                    break
    if label is None:
        raise ValueError("seat provider received an undeclared input")
    with guard:
        with observations.open("a") as output:
            output.write(json.dumps({"label": label, "at_ns": entered,
                "tool_result": current.tool_result, "mail_read": mail,
                "packet": "SEAT_PACKET_TASK" in text,
                "required_read": "SEAT_REQUIRED_MAIL" in text,
                "private_absent": all(value not in text for value in (
                    "NATIVE_FIXTURE_CAPABILITY", "NATIVE_FIXTURE_PRIVATE_CURSOR"))}) + "\n")
    return label


def frames(delay_ms, label, continuing=False):
    continuing = current.tool_result
    if label == "summary":
        text = "## Objective\n- finish the admitted seat fixture task"
    elif label.startswith("msg_seat_") and not continuing:
        return transport.tool_call_frames({"msg_id": label}, 0,
                                          "stateengine_stateengine_msg_read")
    elif label == "SEAT_LAUNCH" and not continuing:
        return transport.tool_call_frames({"path": "seed.txt"}, 0, "read")
    else:
        text = "SEAT_DONE_" + label
    if label == "SEAT_BUSY":
        Path("busy-held").write_text(str(time.monotonic_ns()))
        for _ in range(600):
            if Path("release-busy").exists():
                break
            time.sleep(0.05)
        else:
            raise RuntimeError("fixture busy release exceeded 30 seconds")
        Path("busy-released").write_text(str(time.monotonic_ns()))
    return [("data: " + json.dumps(value) + "\n\n", 0) for value in (
        {"type": "response.output_text.delta", "delta": text},
        {"type": "response.output_text.done"},
        {"type": "response.completed", "response": {"usage": {
            "input_tokens": 30, "output_tokens": 10, "total_tokens": 40}}},
    )]


transport.prompt_of = observe
transport.frames = frames
if __name__ == "__main__":
    raise SystemExit(transport.main())
