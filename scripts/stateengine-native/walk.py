"""Walk packaged native context and restart recovery with disposable sources.

Requires a testkit product build, ordinary native TUI/CLI and the existing
tmux driver. No real credentials or provider endpoints are read or contacted.
"""

import argparse
import base64
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from types import SimpleNamespace
import urllib.request

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


service_helper = load("native_walk_service", SCRIPTS / "native-tui/service.py")
serve_helper = load("native_walk_owner", SCRIPTS / "daily-use/serve_group.py")


def checked(args, **kwargs):
    result = subprocess.run([str(arg) for arg in args], capture_output=True,
                            text=True, timeout=45, **kwargs)
    if result.returncode:
        raise RuntimeError(f"native walk command failed: {args[0]}\n{result.stderr[-2000:]}")
    return result.stdout


def poll(read, description):
    for _ in range(120):
        value = read()
        if value is not None:
            return value
        time.sleep(0.1)
    raise RuntimeError(f"native walk did not observe {description} within 120 bounded polls")


def walk(args):
    evidence = args.evidence.resolve()
    evidence.mkdir(parents=True, mode=0o700)  # Refuse an existing run.
    binaries = evidence / "package/bin"
    binaries.mkdir(parents=True)
    for name in ("grapt-server", "grapt-tui", "grapt"):
        source = args.bin_dir / name
        if not source.is_file() or source.is_symlink() or source.stat().st_size > 1024 ** 3:
            raise RuntimeError(f"native walk binary absent, linked or oversized: {name}")
        if name == "grapt-server":
            marker = b"GRAPT_LIVE_HEADLESS_LOOPBACK"
            found = False
            tail = b""
            with source.open("rb") as stream:
                for _ in range(1024):
                    chunk = stream.read(1024 * 1024)
                    if not chunk:
                        break
                    if marker in tail + chunk:
                        found = True
                        break
                    tail = chunk[-len(marker):]
            if not found:
                raise RuntimeError("native walk requires a loopback-provider-testkit product build")
        shutil.copy2(source, binaries / name)
    (evidence / "package/manifest.json").write_text(json.dumps({
        "purpose": "native context workflow witness", "provider": "disposable scripted fixture",
        "profile": "debug/testkit", "source_commit_claim": args.source_commit,
        "files": [f"bin/{name}" for name in ("grapt-server", "grapt-tui", "grapt")],
    }, indent=2) + "\n", encoding="utf-8")
    inputs = evidence / "dummy-inputs"
    inputs.mkdir()
    config = {
        "store": {"authority": "initialize"}, "product": {"build": "native-context-fixture"},
        "provider": {"id": "openai", "model": "gpt-live", "base_model": "gpt-live",
                     "family": "gpt-5", "route": "route-native", "account": "native-fixture",
                     "credential_version": "fixture-v1", "policy_version": "fixture-v1", "variants": []},
        "catalog": {"source": "models.dev-chatgpt"}, "compaction": {"server_side": False},
        "mcp": {"stateengine": {"type": "local", "codemode": False, "command": [
            sys.executable, str(Path(__file__).with_name("fixture_server.py")), "--mode", "ready"]}},
    }
    (inputs / "grapt.jsonc").write_text(json.dumps(config), encoding="utf-8")
    (inputs / "provider.credential").write_text("native-fixture-dummy", encoding="utf-8")
    (inputs / "model-catalog.json").write_text(json.dumps({"openai": {
        "id": "openai", "name": "OpenAI", "npm": "@ai-sdk/openai", "models": {"gpt-live": {
            "id": "gpt-live", "name": "gpt-live", "family": "gpt-5", "tool_call": True,
            "modalities": {"input": ["text"], "output": ["text"]}, "limit": {"output": 32000}}}}}), encoding="utf-8")
    (inputs / "account-models.json").write_text(json.dumps({"models": [{
        "slug": "gpt-live", "visibility": "list", "context_window": 128000,
        "supported_reasoning_levels": []}]}), encoding="utf-8")
    provider = None
    owned = None
    owned_record = None
    tui = None
    root = evidence / "service"
    logs = []
    try:
        provider_log = (evidence / "provider.log").open("x")
        logs.append(provider_log)
        provider = subprocess.Popen([sys.executable, str(Path(__file__).with_name("fixture_provider.py")),
                                     "--port", "0", "--delay-ms", "0", "--receipt", str(evidence / "provider.json")],
                                    stdin=subprocess.DEVNULL, stdout=provider_log, stderr=provider_log)
        def endpoint():
            path = evidence / "provider.json"
            try:
                return json.loads(path.read_text())["endpoint"] if path.is_file() else None
            except json.JSONDecodeError:
                return None
        os.environ["GRAPT_LIVE_HEADLESS_LOOPBACK"] = poll(endpoint, "fixture endpoint")
        os.environ["GRAPT_TEST_HOME"] = str(evidence / "empty-home")
        os.environ["GRAPT_CONFIG_DIR"] = str(evidence / "empty-config")
        Path(os.environ["GRAPT_TEST_HOME"]).mkdir()
        Path(os.environ["GRAPT_CONFIG_DIR"]).mkdir()
        service_helper.prepare(root, inputs, binaries / "grapt-server", 300000)

        def start_service(index):
            nonlocal owned, owned_record
            pidfile = evidence / f"serve-owner-{index}.json"
            owned = serve_helper.start(SimpleNamespace(binary=binaries / "grapt-server", service=root,
                                       log=evidence / f"serve-{index}.log", pidfile=pidfile, owner=os.getpid()))
            owned_record = json.loads(pidfile.read_text())
            def ready():
                path = root / "serve-ready.json"
                if not path.is_file():
                    return None
                value = json.loads(path.read_text())
                return value if value.get("pid") == owned.pid else None
            return poll(ready, "owned product readiness")

        def stop_service(index):
            nonlocal owned, owned_record
            serve_helper.stop(owned_record)
            owned.wait(timeout=5)
            owned = None
            owned_record = None

        def api(ready, method, path, body=None):
            address = ready["observed_endpoint"]
            encoded = None if body is None else json.dumps(body).encode()
            basic = base64.b64encode(f"{service_helper.USERNAME}:{service_helper.SECRET}".encode()).decode()
            request = urllib.request.Request(address + path, encoded, method=method,
                      headers={"Authorization": "Basic " + basic, "Content-Type": "application/json"})
            with urllib.request.urlopen(request, timeout=5) as reply:
                raw = reply.read(1024 * 1024 + 1)
            if len(raw) > 1024 * 1024:
                raise RuntimeError("native walk API reply exceeds its bound")
            return json.loads(raw) if raw else None

        def driver(command, *tail):
            return checked([args.driver, command, "--session", tui, *tail])

        def start_tui(index):
            nonlocal tui
            tui = checked([args.driver, "start", "--seat", "seat-a", "--journey", f"native-{index}-{os.getpid()}",
                           "--width", "140", "--height", "40", "--cwd", root, "--", binaries / "grapt-tui",
                           "--service", root, "--directory", root]).strip()
            for heading in ("PROJECTS", "AGENTS", "MODELS"):
                driver("wait", "--text", heading, "--timeout-ms", "12000")
                driver("send", "Enter")
            driver("wait", "--text", "enter open", "--timeout-ms", "12000")

        def close_tui():
            nonlocal tui
            driver("send", "C-q")
            def exited():
                state = subprocess.run(["tmux", "display-message", "-p", "-t", tui,
                                        "#{pane_dead} #{pane_dead_status}"],
                                       capture_output=True, text=True, timeout=5)
                if state.returncode:
                    raise RuntimeError("native TUI pane disappeared without its exit witness")
                if state.stdout.strip() == "1 0":
                    return True
                if state.stdout.strip().startswith("1 "):
                    raise RuntimeError("native TUI exited unsuccessfully")
                return None
            poll(exited, "clean native TUI exit")
            # The driver retains dead panes for evidence; explicitly retire
            # this owned session after observing the product's successful exit.
            driver("stop")
            tui = None

        ready = start_service(0)
        start_tui(0)
        driver("send", "n")
        driver("wait", "--text", "enter send", "--timeout-ms", "12000")
        sessions = api(ready, "GET", "/api/session")["data"]
        if len(sessions) != 1:
            raise RuntimeError("native walk expected exactly one created Session")
        session = sessions[0]["id"]
        if args.smoke:
            driver("send", "--text", "Disposable native transport smoke.")
            driver("send", "Enter")
            driver("wait", "--text", "Yes. The native client", "--timeout-ms", "12000")
            (evidence / "smoke-screen.txt").write_text(driver("capture"), encoding="utf-8")
            (evidence / "result.json").write_text(json.dumps({
                "status": "PASS", "scope": "same-harness native transport smoke",
                "sources": "disposable scripted fixture", "real_stateengine_binding": "not measured",
            }, indent=2) + "\n", encoding="utf-8")
            return
        api(ready, "PUT", f"/api/session/{session}/instructions/entries/stateengine-identity", {"value": {
            "scope": "project", "project_id": "native-fixture", "agent_id": "agt_native_fixture"}})
        api(ready, "PUT", f"/api/session/{session}/stateengine/capability", {"capability": "NATIVE_FIXTURE_CAPABILITY"})
        driver("send", "--text", "Native context fixture: inspect working state, memory, repository map and impact.")
        driver("send", "Enter")
        for ordinal, (tool, _) in enumerate(load("native_model_recipe", Path(__file__).with_name("fixture_provider.py")).CALLS, 1):
            call_id = f"call_j04_{ordinal:04d}_{tool}"
            def pending():
                rows = api(ready, "GET", f"/api/session/{session}/permission")["data"]
                return next((row for row in rows if row.get("source", {}).get("callID") == call_id), None)
            ask = poll(pending, f"permission for {tool}")
            driver("wait", "--text", "PERMISSION", "--timeout-ms", "12000")
            driver("wait", "--text", tool, "--timeout-ms", "12000")
            (evidence / f"approval-{tool}.json").write_text(json.dumps(ask, indent=2), encoding="utf-8")
            (evidence / f"approval-{tool}.txt").write_text(driver("capture"), encoding="utf-8")
            driver("send", "o")
        driver("wait", "--text", "Native context tools completed", "--timeout-ms", "12000")
        (evidence / "context-screen.txt").write_text(driver("capture"), encoding="utf-8")
        history = api(ready, "GET", f"/api/session/{session}/message?order=asc&limit=50")
        (evidence / "context-history.json").write_text(json.dumps(history, indent=2), encoding="utf-8")
        close_tui()
        stop_service(0)
        ready = start_service(1)
        start_tui(1)
        driver("send", "Enter")
        driver("send", "Tab")
        driver("wait", "--text", "enter send", "--timeout-ms", "12000")
        driver("send", "--text", "Continue the context inspection after restart.")
        driver("send", "Enter")
        def recovered():
            screen = driver("capture")
            # Match visible copy across the renderer's terminal-width wraps.
            if "Rebind the Session, then send a follow-up prompt." in " ".join(screen.split()):
                return screen
            return None
        restart_screen = poll(recovered, "native restart rebind feedback")
        (evidence / "restart-screen.txt").write_text(restart_screen, encoding="utf-8")
        observed = [json.loads(line.split(" ", 1)[1]) for line in (evidence / "provider.log").read_text().splitlines()
                    if line.startswith("NATIVE_CONTEXT_OBSERVED ")]
        if not observed or observed[-1] != {"result_count": 4, "source_returned": True,
                "stale_preserved": True, "absent_preserved": True, "private_handles_absent": True}:
            raise RuntimeError("native walk did not preserve the observed tool-source evidence")
        (evidence / "result.json").write_text(json.dumps({"status": "PASS", "native_tools": 4,
            "restart_rebind_feedback": True, "sources": "disposable fixtures",
            "real_stateengine_binding": "not measured"}, indent=2) + "\n", encoding="utf-8")
    except Exception:
        # Preserve the actual native interaction before its owned tmux stops.
        if tui:
            try:
                (evidence / "failure-screen.txt").write_text(driver("capture"), encoding="utf-8")
            except (RuntimeError, subprocess.TimeoutExpired):
                pass
        if "session" in locals() and "ready" in locals():
            for name, path in (("history", f"/api/session/{session}/message?order=asc&limit=50"),
                               ("permissions", f"/api/session/{session}/permission")):
                try:
                    (evidence / f"failure-{name}.json").write_text(json.dumps(api(ready, "GET", path), indent=2), encoding="utf-8")
                except (OSError, ValueError):
                    pass
        raise
    finally:
        # Complete every owned cleanup even when one resource refuses to stop.
        try:
            if tui and subprocess.run(["tmux", "has-session", "-t", tui], capture_output=True, timeout=5).returncode == 0:
                checked([args.driver, "stop", "--session", tui])
        finally:
            try:
                if owned and owned_record:
                    serve_helper.stop(owned_record)
                    owned.wait(timeout=5)
            finally:
                try:
                    if provider:
                        provider.terminate()
                        try:
                            provider.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            provider.kill()
                            provider.wait(timeout=5)
                finally:
                    for log in logs:
                        log.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--smoke", action="store_true", help="one native text response before the context walk")
    parser.add_argument("--bin-dir", required=True, type=Path)
    parser.add_argument("--driver", required=True, type=Path)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--evidence", required=True, type=Path)
    walk(parser.parse_args())
