#!/usr/bin/env python3
"""Real scratch serve/MCP/TUI lifecycle witness, including the drop-trap mutant.

Requires explicit retained binary/profile inputs; missing evidence fails loudly.
No LLM call is made. The profile is copied, never opened or changed in place.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import time
from urllib.parse import urlparse


HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("serve_group", HERE / "serve_group.py")
group = importlib.util.module_from_spec(spec)
spec.loader.exec_module(group)


def prepare(service, template):
    service.mkdir()
    for name in ("database.sqlite3", "service.conf", "account-models.json",
                 "model-catalog.json", "provider.credential", "basic.credential"):
        shutil.copyfile(template / name, service / name)
        (service / name).chmod(0o600)
    config = json.loads((template / "grapt.jsonc").read_text())
    # This is a copied committed store, not a new first-contact profile.
    config["store"]["authority"] = "open-ready"
    config["mcp"]["servers"]["journey"]["command"] = [
        "/usr/bin/node", str(HERE / "journey_mcp_server.js"),
        str(service / "mcp-transcript.jsonl"),
    ]
    (service / "grapt.jsonc").write_text(json.dumps(config, indent=2) + "\n")


def assert_cleanup(service, snapshot):
    members = group.live_members(snapshot["owner"]["pid"])
    if members:
        raise AssertionError(f"serve/MCP group survived driver exit: {members}")
    needle = os.fsencode(str(service))
    with os.scandir("/proc") as entries:
        for index, entry in enumerate(entries):
            if index >= 32768:
                raise AssertionError("service process census exceeded32768")
            if not entry.name.isdecimal():
                continue
            argv = group.command(int(entry.name), 65536)
            if argv is not None and needle in argv:
                raise AssertionError(f"service-dir process survived: {entry.name}")
    endpoint = urlparse(json.loads((service / "serve-ready.json").read_text())["observed_endpoint"])
    with socket.socket() as probe:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        probe.bind((endpoint.hostname, endpoint.port))
        probe.listen(1)


def run_case(args, name, *, fault=False, interrupt=None, mutant=False):
    case = args.evidence / name
    case.mkdir()
    service = case / "service"
    prepare(service, args.profile)
    driver = HERE / "mcp_pane.sh"
    if mutant:
        mutation = case / "mutant"
        mutation.mkdir()
        for filename in ("serve_group.py", "evidence_serve.sh", "mcp_pane.sh"):
            shutil.copyfile(HERE / filename, mutation / filename)
        source = mutation / "evidence_serve.sh"
        text = source.read_text()
        target = "trap evidence_serve_cleanup EXIT\n"
        assert text.count(target) == 1, "drop-trap target must occur exactly once"
        source.write_text(text.replace(target, "# mutant: EXIT cleanup omitted\n"))
        driver = mutation / "mcp_pane.sh"
    environment = os.environ.copy()
    environment.pop("STATEENGINE_AGENT_ID", None)
    environment.pop("STATEENGINE_PROJECT_ID", None)
    environment["EVIDENCE_FAIL_AFTER_MCP"] = "1" if fault else "0"
    environment["EVIDENCE_PAUSE_AFTER_MCP"] = "1" if interrupt else "0"
    evidence = case / "captures"
    snapshot = None
    with (case / "driver.log").open("w") as log:
        child = subprocess.Popen(["bash", str(driver), str(args.binary_dir), str(service),
                                  str(args.repository), str(evidence), "valid"],
                                 env=environment, stdout=log, stderr=subprocess.STDOUT)
        try:
            if interrupt:
                for _ in range(400):
                    if (evidence / "live-processes.json").exists():
                        child.send_signal(interrupt)
                        break
                    if child.poll() is not None:
                        raise AssertionError("driver exited before signal witness")
                    time.sleep(.1)
                else:
                    raise AssertionError("driver never reached connected MCP within40s")
            rc = child.wait(timeout=45)
            expected = 128 + interrupt if interrupt else 23 if fault else 0
            assert rc == expected, f"{name}: driver exit{rc}, expected{expected}; see{case}/driver.log"
            snapshot = json.loads((evidence / "live-processes.json").read_text())
            assert any("journey_mcp_server.js" in row["argv"] for row in snapshot["members"]), \
                "connected stdio MCP child was not observed in the owned serve group"
            assert any("grapt-server" in row["argv"] for row in snapshot["members"]), \
                "real serve was not observed in its owned group"
            if mutant:
                try:
                    assert_cleanup(service, snapshot)
                except AssertionError as error:
                    result = {"name": name, "rc": rc, "mutant": "KILLED", "reason": str(error)}
                else:
                    raise AssertionError("drop-trap mutant survived the lifecycle witness")
            else:
                assert_cleanup(service, snapshot)
                assert not (evidence / "serve-owner.json").exists(), "pidfile survived successful teardown"
                result = {"name": name, "rc": rc, "cleanup": "PASS", "ports": "FREE"}
            (case / "result.json").write_text(json.dumps(result, indent=2) + "\n")
            return result
        finally:
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=12)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
            # Bound the mutant's lifetime and any failing witness. Only this
            # case's explicitly recorded group and tmux session may be stopped.
            receipt = evidence / "serve-owner.json"
            if receipt.exists():
                record = json.loads(receipt.read_text())
                group.stop(record)
                receipt.unlink()
            subprocess.run(["tmux", "kill-session", "-t", f"mcp-pane-{child.pid}"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
            if snapshot:
                assert_cleanup(service, snapshot)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary-dir", type=Path, required=True)
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--repository", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args()
    for attribute in ("binary_dir", "profile", "repository"):
        setattr(args, attribute, getattr(args, attribute).resolve(strict=True))
    args.evidence = args.evidence.resolve()
    args.evidence.mkdir() # Existing evidence is never overwritten.
    results = [run_case(args, "normal"), run_case(args, "forced-failure", fault=True),
               run_case(args, "term", interrupt=signal.SIGTERM),
               run_case(args, "int", interrupt=signal.SIGINT),
               run_case(args, "drop-trap", fault=True, mutant=True),
               run_case(args, "restored", fault=True)]
    (args.evidence / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    print(json.dumps(results))


if __name__ == "__main__":
    main()
