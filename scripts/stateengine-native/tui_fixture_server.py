"""Synthetic full-message reads/replies for the native-TUI acceptance journey."""
import argparse
import json
from pathlib import Path

from acceptance_peer import load

fixture = load("tui_native_mcp", Path(__file__).with_name("fixture_server.py"))
MESSAGES = ("msg_tui_idle", "msg_tui_busy", "msg_tui_restart")


def install(events, busy_marker):
    original = fixture.result

    def result(name, args, mode):
        if name == "stateengine_msg_read":
            identifier = args.get("msg_id")
            if args != {"msg_id": identifier} or identifier not in MESSAGES:
                raise ValueError("foreign TUI fixture message")
            value = {"message": {"msg_id": identifier, "from": "agt_native_sender",
                "body": "FULL_BODY_END_" + identifier}, "read_acknowledgement": "recorded",
                "delivery_acknowledgement": "unchanged", "dismissal": "unchanged"}
        elif name == "stateengine_msg_send":
            identifier = args.get("body", "").removeprefix("Read ")
            if identifier not in MESSAGES or args != {"to": "agt_native_sender",
                    "kind": "handoff", "body": "Read " + identifier}:
                raise ValueError("foreign TUI fixture reply")
            value = {"msg_id": "reply_" + identifier, "to": "agt_native_sender", "sent": True}
        else:
            return original(name, args, mode)
        with events.open("a", encoding="utf-8") as log:
            log.write(json.dumps({"tool": name, "message": identifier,
                                  "while_busy": busy_marker.exists()}) + "\n")
        return value

    fixture.result = result


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=["ready"], default="ready")
    parser.add_argument("--calls-file", required=True)
    parser.add_argument("--tui-events", type=Path, required=True)
    parser.add_argument("--busy-marker", type=Path, required=True)
    args = parser.parse_args()
    install(args.tui_events, args.busy_marker)
    fixture.serve(args.mode, args.calls_file)
