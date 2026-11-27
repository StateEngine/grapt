"""Explicit live schema smoke; counts tokens without generating a message.

First export the tools from the real product regression using
GRAPT_TEST_ANTHROPIC_TOOLS_EXPORT, then run this script with --tools PATH.
The approved API key is read privately; no key copy or request capture is made.
"""
import argparse
import json
from pathlib import Path
import stat
import urllib.error
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--tools", required=True, type=Path)
parser.add_argument("--key-file", type=Path, default=Path.home() / ".config/grapt-bench/anthropic-api.key")
args = parser.parse_args()
key_info = args.key_file.stat()
if not stat.S_ISREG(key_info.st_mode) or key_info.st_mode & 0o077 or key_info.st_size > 1024:
    raise SystemExit("API key requires a private regular file within 1 KiB")
with args.key_file.open() as source:
    key = source.read(1025).strip()
if not key.startswith("sk-ant-") or any(character.isspace() for character in key):
    raise SystemExit("Approved Anthropic API key file is malformed")
with args.tools.open("rb") as source:
    raw = source.read(4 * 1024 * 1024 + 1)
if len(raw) > 4 * 1024 * 1024 or b"sk-ant-" in raw:
    raise SystemExit("Tools export is oversized or contains credential material")
tools = json.loads(raw)
if not isinstance(tools, list) or not 0 < len(tools) <= 1024:
    raise SystemExit("Expected a bounded, nonempty tools array")
body = json.dumps({"model":"claude-haiku-5-5", "messages":[{"role":"user","content":"hello"}], "tools":tools}).encode()
request = urllib.request.Request("https://api.anthropic.com/v1/messages/count_tokens", data=body,
    headers={"x-api-key":key,"anthropic-version":"2023-06-01","content-type":"application/json"}, method="POST")
class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None
opener = urllib.request.build_opener(NoRedirect(), urllib.request.ProxyHandler({}))
try:
    with opener.open(request, timeout=30) as response:
        status, result = response.status, response.read(65537)
except urllib.error.HTTPError as error:
    status, result = error.code, error.read(65537)
except (OSError, urllib.error.URLError):
    raise SystemExit("Anthropic count_tokens transport failed; no inference was requested") from None
if len(result) > 65536 or b"sk-ant-" in result or key.encode() in result:
    raise SystemExit("Provider reply exceeded its bound or contained credential material; not reported")
value = json.loads(result)
count = value.get("input_tokens")
passed = status == 200 and type(count) is int and count > 0
report = {"status":status,"validated":passed,"tool_count":len(tools),"input_tokens":count if passed else None,"key_copies_created":False}
if not passed:
    report["error_type"] = str(value.get("error",{}).get("type","unknown"))[:128]
    report["message"] = str(value.get("error",{}).get("message","no diagnostic"))[:320]
print(json.dumps(report))
raise SystemExit(0 if passed else 1)
