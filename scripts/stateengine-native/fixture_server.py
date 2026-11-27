"""Disposable stdio MCP source for native continuity/memory/Atlas witnesses.

This is fixture data, not a StateEngine provider release or a mail adapter.
It uses only a dummy session capability and never logs incoming metadata.
"""

import argparse
import json
import os
from pathlib import Path
import sys
import time

MAX_LINE = 65536
MAX_REPLY = 32768
CAPABILITY = "NATIVE_FIXTURE_CAPABILITY"
PROJECT = "native-fixture"
AGENT = "agt_native_fixture"
SOURCE_TOKEN = "NATIVE_SOURCE_VICTOR6284"
MAIL_READ = False
REQUIRED_INBOX_READ = False


def tool(name, description, properties):
    return {
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties},
    }


INBOX_TOOL = tool("stateengine_msg_list", "Read the exact bound fixture inbox.", {
    "unread_only": {"type": "boolean"}, "select": {"type": "string"},
    "limit": {"type": "integer"}, "before": {"type": "string"},
})

TOOLS = [
    tool("stateengine_resume", "Resume bound fixture continuity; harness owned.", {
        "since": {"type": "string"}, "byte_budget": {"type": "integer"},
        "packet_id": {"type": "string"},
    }),
    tool("stateengine_working_state_get", "Read the fixture's current working state.", {}),
    tool("stateengine_memory_recall", "Recall fixture memory; ratification is explicit.", {
        "query": {"type": "string"}, "limit": {"type": "integer"},
    }),
    tool("stateengine_atlas_repo_map", "Get fixture repository pointers and index freshness.", {
        "query": {"type": "string"}, "token_budget": {"type": "integer"},
    }),
    tool("stateengine_atlas_impact", "Get fixture impact edges and their provenance.", {
        "target": {"type": "string"}, "limit": {"type": "integer"},
    }),
    tool("stateengine_msg_read", "Read one synthetic mail body for the bound fixture principal.", {
        "msg_id": {"type": "string"},
    }),
    tool("stateengine_msg_ack", "Acknowledge the synthetic mail after reading it.", {
        "msg_id": {"type": "string"},
    }),
    tool("stateengine_msg_send", "Reply inside this disposable fixture only.", {
        "to": {"type": "string"}, "kind": {"type": "string"}, "body": {"type": "string"},
    }),
]


def result(name, args, mode):
    global MAIL_READ, REQUIRED_INBOX_READ
    if name == "stateengine_resume":
        reply = {
            "schema_version": 2,
            "resume_status": "ready",
            "complete": mode != "incomplete",
            "principal": {
                "project_id": PROJECT,
                "agent_id": "agt_foreign" if mode == "foreign" else AGENT,
                "verified": True,
                "identity_fingerprint": "native-fixture-fingerprint",
            },
            "cursor": "NATIVE_FIXTURE_PRIVATE_CURSOR",
            "required_reads": [],
            "evidence_health": {"rendering": {"render_truncated": mode == "stale"}},
            "continuity": {"source": "disposable-fixture", "text": SOURCE_TOKEN},
        }
        if mode == "seat-required-reads":
            if "since" not in args:
                REQUIRED_INBOX_READ = False
                reply["resume_status"] = "needs_read"
                reply["required_reads"] = [{"tool": "stateengine_msg_list", "arguments": {
                    "unread_only": True, "select": "newest", "limit": 20}}]
            elif not REQUIRED_INBOX_READ:
                raise ValueError("required fixture inbox read was omitted")
        if "packet_id" in args:
            if args["packet_id"] != "native_seat_packet":
                raise ValueError("fixture received the wrong launch packet")
            reply["packet_acknowledgement"] = {
                "state": "acknowledged", "recorded": True,
                "packet_id": "foreign_packet" if mode == "packet-bad-ack" else args["packet_id"],
            }
        return reply
    if name == "stateengine_msg_list":
        if mode != "seat-required-reads" or args != {"unread_only": True, "select": "newest", "limit": 20}:
            raise ValueError("fixture required-read query changed")
        REQUIRED_INBOX_READ = True
        return {"complete": True, "messages": [{"body": "SEAT_REQUIRED_MAIL"}]}
    provenance = {"source": "disposable-fixture", "token": SOURCE_TOKEN}
    if name == "stateengine_working_state_get":
        return {"working_state": "repair calc", "ratified": False, "provenance": provenance}
    if name == "stateengine_memory_recall":
        return {"memories": [{"text": "calc rounds down", "ratified": False}], "provenance": provenance}
    if name == "stateengine_atlas_repo_map":
        return {"pointers": ["src/calc.rs"], "contents_included": False,
                "freshness": "fixture-index-current", "provenance": provenance}
    if name == "stateengine_atlas_impact":
        return {"target": args.get("target"), "edges": [],
                "freshness": "fixture-index-stale", "complete": False,
                "reason": "fixture edge index absent", "provenance": provenance}
    if name == "stateengine_msg_read":
        if mode == "seat-required-reads" and args in (
                {"msg_id": "msg_seat_idle"}, {"msg_id": "msg_seat_busy"}):
            return {"message": {"msg_id": args["msg_id"], "body": "SEAT_MAIL_" +
                    args["msg_id"].removeprefix("msg_seat_").upper()}, "read_acknowledgement": "recorded"}
        if args != {"msg_id": "msg_native_long"}:
            raise ValueError("foreign fixture message")
        MAIL_READ = True
        return {"message": {"msg_id": "msg_native_long", "from": "agt_native_sender",
                            "body": "COMPACT_PREVIEW " + "bounded full body 🦀 " * 30 + "FULL_BODY_END"},
                "read_acknowledgement": "recorded", "delivery_acknowledgement": "unchanged",
                "dismissal": "unchanged", "provenance": provenance}
    if name == "stateengine_msg_ack":
        if mode != "codemode-mail" or not MAIL_READ or args != {"msg_id": "msg_native_long"}:
            raise ValueError("fixture acknowledgement requires its full message read")
        return {"msg_id": "msg_native_long", "delivery_acknowledgement": "recorded",
                "provenance": provenance}
    if name == "stateengine_msg_send":
        if args != {"to": "agt_native_sender", "kind": "handoff", "body": "Full message read; fixture reply."}:
            raise ValueError("fixture reply scope or body")
        return {"msg_id": "msg_native_reply", "to": "agt_native_sender", "sent": True,
                "provenance": provenance}
    raise ValueError("unknown fixture tool")


def serve(mode, calls_file=None):
    initialized = False
    for _ in range(128):
        line = sys.stdin.buffer.readline(MAX_LINE + 1)
        if not line:
            return
        if len(line) > MAX_LINE or not line.endswith(b"\n"):
            raise ValueError("fixture input exceeds its line bound")
        message = json.loads(line)
        error = None
        method = message.get("method")
        if method == "notifications/initialized":
            initialized = True
            continue
        if "id" not in message:
            continue
        if method == "initialize":
            if mode == "startup-silent":
                # Keep stdio open until the owner cancels this startup.
                for _ in range(128):
                    if not sys.stdin.buffer.readline(MAX_LINE + 1):
                        return
                raise ValueError("silent fixture input-count bound reached")
            body = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                    "serverInfo": {"name": "native-stateengine-fixture", "version": "1"}}
        elif not initialized:
            raise ValueError("fixture handshake omitted initialized notification")
        elif method == "tools/list":
            body = {"tools": TOOLS + ([INBOX_TOOL] if mode in ("seat-required-reads", "census-failures") else [])}
        elif method == "tools/call":
            entered = time.monotonic_ns()
            params = message["params"]
            if calls_file:
                with open(calls_file, "a", encoding="utf-8") as calls:
                    calls.write(params["name"] + "\n")
            if params.get("_meta", {}).get("stateengine.session-capability-v1") != CAPABILITY:
                raise ValueError("fixture requires its dummy session binding")
            args = params.get("arguments", {})
            if any(key in args for key in ("agent_id", "project_id")):
                raise ValueError("fixture refuses caller-supplied principal overrides")
            if len(json.dumps(args).encode()) > 4096:
                raise ValueError("fixture arguments exceed their bound")
            if mode == "census-failures" and params["name"] in ("stateengine_msg_send", "stateengine_msg_list"):
                if args.get("protocol"):
                    detail = "Error: nested protocol refused " + json.dumps(args["nested"])
                    error = {"code": -32041, "message": detail}
                elif "nested" in args:
                    detail = "Error: nested request refused " + json.dumps(args["nested"]) + ('"\\' * 800) + "x" * 7000
                elif params["name"] == "stateengine_msg_send":
                    detail = "Error: kind 'task-pointer' requires at least one ref (messages never assign work)"
                elif "before" in args:
                    detail = "Error: before/retry_key on msg_list require dismiss=true"
                else:
                    detail = "bounded oversized inbox result " + "x" * 1_100_000
                body = {"isError": params["name"] == "stateengine_msg_send" or "before" in args,
                        "content": [{"type": "text", "text": detail}]}
            elif mode == "transport":
                body = {"isError": True, "content": [{"type": "text", "text": "hostile fixture transport text"}]}
            else:
                value = result(params["name"], args, mode)
                if calls_file and mode == "seat-required-reads":
                    with open(str(calls_file) + ".jsonl", "a", encoding="utf-8") as calls:
                        calls.write(json.dumps({"name": params["name"], "arguments": args,
                            "result": value, "at_ns": entered}) + "\n")
                body = {"isError": False, "content": [{"type": "text", "text": json.dumps(value)}]}
        else:
            raise ValueError("unknown fixture method")
        reply = json.dumps({"jsonrpc": "2.0", "id": message["id"],
                            **({"error": error} if error else {"result": body})})
        if len(reply.encode()) > (1_200_000 if mode == "census-failures" else MAX_REPLY):
            raise ValueError("fixture reply exceeds its bound")
        print(reply, flush=True)
    raise ValueError("fixture request-count bound reached")


def fail_first(path):
    """A disposable fixture records spawns and refuses exactly its first one."""
    path = Path(path)
    try:
        with path.open("r", encoding="utf-8") as source:
            text = source.read(32)
        if len(text) > 16:
            raise ValueError("fixture spawn counter exceeds bound")
        count = int(text)
        if not 1 <= count < 128:
            raise ValueError("fixture spawn counter out of range")
    except FileNotFoundError:
        count = 0
    path.write_text(str(count + 1), encoding="utf-8")
    if count == 0:
        print("first startup exited: " + os.environ.get("SEAT04_FIXTURE_SECRET", "fixture-only"),
              file=sys.stderr, flush=True)
        return True
    return False


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("ready", "foreign", "incomplete", "stale", "transport", "packet-bad-ack", "codemode-mail", "census-failures", "startup-silent", "seat-required-reads"), default="ready")
    parser.add_argument("--project", default=PROJECT)
    parser.add_argument("--agent", default=AGENT)
    parser.add_argument("--capability", default=CAPABILITY)
    parser.add_argument("--calls-file")
    parser.add_argument("--fail-first-file")
    args = parser.parse_args()
    PROJECT, AGENT, CAPABILITY = args.project, args.agent, args.capability
    if args.fail_first_file and fail_first(args.fail_first_file):
        sys.exit(17)
    serve(args.mode, args.calls_file)
