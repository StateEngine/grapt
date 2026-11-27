"""Bounded stdio peer advertising the public StateEngine core tool fixture."""
import json
import pathlib
import sys

catalog = json.loads(pathlib.Path(__file__).with_name("stateengine-core-tools.json").read_text())
for _ in range(64):
    line = sys.stdin.readline(65537)
    if not line:
        break
    if len(line) > 65536:
        raise SystemExit("request exceeds 64 KiB")
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"], "capabilities": {"tools": {}}, "serverInfo": {"name": "schema-fixture", "version": "1"}}
    elif method == "tools/list":
        result = catalog
    else:
        if method == "tools/call":
            pathlib.Path(sys.argv[1]).write_text("tools/call reached the server")
        result = {"content": [{"type": "text", "text": "fixture call"}]}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
