"""Product-local stdio MCP peer for cancellation and stderr regressions."""
import json
import os
import sys
import time

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if "id" not in request:
        continue
    while not os.path.exists(sys.argv[1]):
        # Keep the handshake in flight until released or its child is killed.
        time.sleep(0.005)
    # Larger than either platform's pipe capacity, before EVERY reply.
    sys.stderr.write("diagnostic fixture\n" * 8192)
    sys.stderr.flush()
    if method == "initialize":
        result = {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "cancellation-fixture", "version": "1"},
        }
    elif method == "tools/list":
        result = {"tools": [{"name": "fixture_read", "inputSchema": {"type": "object"}}]}
    elif method == "tools/call":
        if request["params"].get("arguments", {}).get("silent"):
            continue
        if log := os.environ.get("MCP_CALL_LOG"):
            with open(log, "a", encoding="utf-8") as recorded:
                recorded.write(str(request["id"]) + "\n")
        result = {"content": [{"type": "text", "text": "current"}]}
    else:
        raise RuntimeError("unexpected fixture request: " + method)
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
