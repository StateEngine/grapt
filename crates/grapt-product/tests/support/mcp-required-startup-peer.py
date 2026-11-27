"""The first local MCP process times out; its successor answers normally."""
import json
import os
import sys
import time

try:
    fd = os.open(sys.argv[1], os.O_CREAT | os.O_EXCL | os.O_WRONLY)
except FileExistsError:
    first = False
else:
    os.close(fd)
    first = True
for _ in range(128):
    line = sys.stdin.buffer.readline(65537)
    if not line:
        break
    if len(line) > 65536 or not line.endswith(b"\n"):
        raise RuntimeError("fixture input bound")
    request = json.loads(line)
    method = request.get("method")
    if "id" not in request:
        continue
    if ((first and sys.argv[2] == "first-handshake" and method == "initialize")
            or (sys.argv[2] == "all-handshakes" and method == "initialize")
            or (first and sys.argv[2] == "first-discovery" and method == "tools/list")):
        time.sleep(60)  # the client's declared handshake deadline must kill this child
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "required-startup-fixture", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "stateengine_resume", "inputSchema": {"type": "object"}}]}
    else:
        raise RuntimeError("unexpected fixture request")
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
