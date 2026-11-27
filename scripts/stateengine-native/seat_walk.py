"""Run headless and native-TUI seat journeys in a private network namespace.

Only StateEngine and the model are synthetic. The service, provision command,
clients, packet acknowledgement and native v2 mailbox path are the real ones.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import time
from types import SimpleNamespace
import urllib.request

sys.dont_write_bytecode = True
from walk import checked, poll, service_helper, serve_helper
from seat_mail_fixture import MailPeer
from seat_assertions import run_joined

HERE = Path(__file__).resolve().parent
SESSION = "ses_native_seat_surface"
PACKET = b"packet_id: native_seat_packet\nSEAT_PACKET_TASK: finish the admitted fixture task.\n"


class Journey:
    def __init__(self, evidence, binaries, client):
        self.evidence, self.binaries, self.client = evidence, binaries, client
        self.root = evidence / "service"
        self.provider = self.owned = self.owned_record = self.tui = self.peer = None
        self.logs = []
        self.clients = []

    def api(self, method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        basic = base64.b64encode(f"{service_helper.USERNAME}:{service_helper.SECRET}".encode()).decode()
        request = urllib.request.Request(self.ready["observed_endpoint"] + path, data, method=method,
            headers={"Authorization": "Basic " + basic, "Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=5) as reply:
            raw = reply.read(1024 * 1024 + 1)
        if len(raw) > 1024 * 1024:
            raise ValueError("seat API reply bound")
        return json.loads(raw) if raw else None

    def driver(self, operation, *args):
        return checked([self.binaries / "grapt-tui-driver", operation, "--session", self.tui, *args])

    def observations(self):
        path = self.evidence / "provider-observations.jsonl"
        if not path.exists():
            return []
        raw = path.read_text()
        if len(raw) > 65536:
            raise ValueError("provider observation bound")
        return [json.loads(line) for line in raw.splitlines()]

    def done(self, label):
        marker = "SEAT_DONE_" + label
        history = self.api("GET", f"/api/session/{SESSION}/message?order=asc&limit=50")
        return history if marker in json.dumps(history) else None

    def run_input(self, text, wait=True):
        if self.client == "tui":
            self.driver("send", "--text", "\x1b[200~" + text + "\x1b[201~")
            self.driver("wait", "--text", text, "--timeout-ms", "12000")
            self.driver("send", "Enter")
            child = None
        else:
            log = (self.evidence / (text + ".log")).open("x")
            self.logs.append(log)
            child = subprocess.Popen([self.binaries / "grapt", "run", "--service", self.root,
                "--session", SESSION, "--prompt", text, "--jsonl"],
                stdin=subprocess.DEVNULL, stdout=log, stderr=log, cwd=self.root)
            self.clients.append(child)
        if not wait:
            return child
        poll(lambda: self.done(text), text + " completion")
        if child and child.wait(timeout=5) != 0:
            raise RuntimeError("headless input failed")

    def prepare(self):
        self.evidence.mkdir()
        inputs = self.evidence / "inputs"
        inputs.mkdir()
        config = {
            "store": {"authority": "initialize"}, "product": {"build": "seat-surface-fixture"},
            "provider": {"id": "openai", "model": "gpt-live", "base_model": "gpt-live",
                "family": "gpt-5", "route": "route-native", "account": "native-fixture",
                "credential_version": "fixture-v1", "policy_version": "fixture-v1", "variants": []},
            "catalog": {"source": "models.dev-chatgpt"}, "compaction": {"server_side": False},
            "permission": {"read": "allow", "stateengine_*": "allow"},
            "mcp": {"stateengine": {"type": "local", "codemode": False, "command": [
                sys.executable, str(HERE / "fixture_server.py"), "--mode", "seat-required-reads",
                "--calls-file", str(self.evidence / "mcp-calls.txt")]}}}
        (inputs / "grapt.jsonc").write_text(json.dumps(config))
        (inputs / "provider.credential").write_text("native-fixture-dummy")
        (inputs / "model-catalog.json").write_text(json.dumps({"openai": {
            "id": "openai", "name": "OpenAI", "npm": "@ai-sdk/openai", "models": {"gpt-live": {
                "id": "gpt-live", "name": "gpt-live", "family": "gpt-5", "tool_call": True,
                "modalities": {"input": ["text"], "output": ["text"]}, "limit": {"output": 32000}}}}}))
        (inputs / "account-models.json").write_text(json.dumps({"models": [{"slug": "gpt-live",
            "visibility": "list", "context_window": 128000, "supported_reasoning_levels": []}]}))
        log = (self.evidence / "provider.log").open("x")
        self.logs.append(log)
        self.provider = subprocess.Popen([sys.executable, HERE / "seat_provider_fixture.py",
            "--port", "0", "--delay-ms", "0", "--receipt", self.evidence / "provider.json"],
            stdin=subprocess.DEVNULL, stdout=log, stderr=log, cwd=self.evidence)
        receipt = self.evidence / "provider.json"
        os.environ["GRAPT_LIVE_HEADLESS_LOOPBACK"] = poll(
            lambda: json.loads(receipt.read_text())["endpoint"] if receipt.exists() else None,
            "provider endpoint")
        service_helper.prepare(self.root, inputs, self.binaries / "grapt-server", 300000)
        (self.root / "seed.txt").write_text("synthetic seed\n" * 600)
        self.peer = MailPeer(HERE.parents[1] / "fixtures/stateengine-native/stateengine-mail-profile.json")
        pidfile = self.evidence / "serve-owner.json"
        self.owned = serve_helper.start(SimpleNamespace(binary=self.binaries / "grapt-server",
            service=self.root, log=self.evidence / "service.log", pidfile=pidfile, owner=os.getpid()))
        self.owned_record = json.loads(pidfile.read_text())
        def ready():
            path = self.root / "serve-ready.json"
            if path.exists():
                value = json.loads(path.read_text())
                if value.get("pid") == self.owned.pid:
                    return value
            return None
        self.ready = poll(ready, "owned service readiness")
        packet = self.evidence / "packet.md"
        packet.write_bytes(PACKET)
        pointer = {"state": "written", "path": str(packet), "bytes": len(PACKET),
                   "sha256": hashlib.sha256(PACKET).hexdigest()}
        identity = {"scope": "project", "project_id": "native-fixture", "agent_id": "agt_native_fixture",
                    "mail_transport_protocol": "remote-seat-mail-transport/v2"}
        checked([self.binaries / "grapt", "session", "provision", "--service", self.root,
            "--session", SESSION, "--agent", "agt_live_headless1", "--model", "openai/gpt-live", "--location", self.root,
            "--identity-entry", json.dumps(identity), "--packet-entry", json.dumps(pointer)],
            input="NATIVE_FIXTURE_CAPABILITY\n", cwd=self.root)
        if self.client == "tui":
            self.tui = checked([self.binaries / "grapt-tui-driver", "start", "--seat", "seat-a",
                "--journey", "seat-surface-" + str(os.getpid()), "--width", "140", "--height", "40",
                "--cwd", self.root, "--", self.binaries / "grapt-tui", "--service", self.root,
                "--session", SESSION]).strip()
            def catalog_ready():
                screen = self.driver("capture")
                return True if ("Catalog:" not in screen and "Catalog refresh" not in screen
                    and "agent agt_live_headless1" in screen and "model openai/gpt-live" in screen) else None
            poll(catalog_ready, "saved TUI choices after catalog refresh")
            self.run_input("SEAT_LAUNCH")
        else:
            self.run_input("SEAT_LAUNCH")

    def witness(self):
        self.prepare()
        poll(lambda: self.done("SEAT_LAUNCH"), "initial packet completion")
        calls = (self.evidence / "mcp-calls.txt").read_text().splitlines()
        assert calls == ["stateengine_resume", "stateengine_msg_list", "stateengine_resume"], calls
        (self.evidence / "packet.md").unlink()
        self.peer.publish("msg_seat_idle")
        poll(lambda: self.done("msg_seat_idle"), "idle mail execution")
        child = self.run_input("SEAT_BUSY", wait=False)
        try:
            poll(lambda: any(row["label"] == "SEAT_BUSY" for row in self.observations()) or None,
                 "busy provider entry")
            poll(lambda: True if (self.evidence / "busy-held").exists() else None,
                 "held busy response")
            self.held_after = time.monotonic_ns()
            self.peer.publish("msg_seat_busy")
            # Keep a real interval, recorded by every provider, MCP and mail
            # attempt; final validation checks the entire interval after join.
            for _ in range(20):
                time.sleep(0.05)
            assert self.done("SEAT_BUSY") is None, "busy execution escaped its fixture barrier"
        finally:
            (self.evidence / "release-busy").touch()
        poll(lambda: self.done("msg_seat_busy"), "queued mail execution")
        poll(lambda: self.peer.mailbox.acknowledged == {"msg_seat_idle", "msg_seat_busy"} or None,
             "both mail delivery acknowledgements")
        assert self.peer.mailbox.fetched == {"msg_seat_idle", "msg_seat_busy"}
        if child and child.wait(timeout=5) != 0:
            raise RuntimeError("busy headless input failed")
        self.api("POST", f"/api/session/{SESSION}/compact", {})
        self.run_input("SEAT_COMPACTED")
        with sqlite3.connect(self.root.joinpath("database.sqlite3").as_uri() + "?mode=ro", uri=True) as db:
            compacted = db.execute("SELECT COUNT(*) FROM event WHERE type = ? AND aggregate_id = ?",
                ("session.compaction.ended.2", SESSION)).fetchone()[0]
        assert compacted == 1, "no actual committed compaction"
        if self.tui:
            self.driver("wait", "--text", "SEAT_DONE_SEAT_COMPACTED", "--timeout-ms", "12000")
            (self.evidence / "screen.txt").write_text(self.driver("capture"))
            self.driver("send", "C-q")
            poll(lambda: checked(["tmux", "display-message", "-p", "-t", self.tui,
                                  "#{pane_dead} #{pane_dead_status}"]).strip() == "1 0" or None,
                 "clean TUI exit")
            self.driver("stop")
            self.tui = None
        return {"client": self.client, "packet_reused_after_removal": True,
                "idle_wake": True, "busy_mail_queued": True, "compaction_refresh": True}

    def close(self):
        try:
            if self.tui:
                self.driver("stop")
        finally:
            try:
                for child in self.clients:
                    if child.poll() is None:
                        child.terminate()
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=5)
                if self.owned and self.owned_record:
                    serve_helper.stop(self.owned_record)
                    self.owned.wait(timeout=5)
            finally:
                try:
                    if self.peer:
                        self.peer.close()
                        mailbox = self.peer.mailbox
                        (self.evidence / "mailbox.json").write_text(json.dumps({
                            "calls": mailbox.calls, "error": mailbox.error,
                            "requests": mailbox.requests,
                            "acknowledged": sorted(mailbox.acknowledged),
                            "fetched": sorted(mailbox.fetched),
                            "snapshots": {key: [row["source_message"] for row in rows]
                                for key, rows in mailbox.snapshots.items()}}, indent=2) + "\n")
                finally:
                    if self.provider:
                        self.provider.terminate()
                        try:
                            self.provider.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            self.provider.kill()
                            self.provider.wait(timeout=5)
                    for log in self.logs:
                        log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--inside-netns", default=None, help=argparse.SUPPRESS)
    args = parser.parse_args()
    evidence = args.evidence.resolve()
    if args.inside_netns:
        if os.readlink("/proc/self/ns/net") == args.inside_netns:
            raise RuntimeError("host network namespace refused")
        results = []
        for client in ("headless", "tui"):
            journey = Journey(evidence / client, args.bin_dir, client)
            results.append(run_joined(journey))
        (evidence / "result.json").write_text(json.dumps({"status": "PASS", "cases": results,
            "sources": "disposable stubs; live StateEngine not measured"}, indent=2) + "\n")
        return
    evidence.mkdir(mode=0o700)
    binaries = evidence / "bin"
    binaries.mkdir()
    for name in ("grapt", "grapt-server", "grapt-tui", "grapt-tui-driver"):
        source = args.bin_dir / name
        if not source.is_file() or source.is_symlink() or source.stat().st_size > 1024 ** 3:
            raise ValueError("seat fixture requires regular packaged binaries: " + name)
        shutil.copy2(source, binaries / name)
    env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "TERM": "xterm-256color",
           "PYTHONDONTWRITEBYTECODE": "1"}
    for variable, name in (("HOME", "home"), ("GRAPT_TEST_HOME", "test-home"),
                           ("GRAPT_CONFIG_DIR", "config"), ("TMUX_TMPDIR", "tmux")):
        path = evidence / name
        path.mkdir(mode=0o700)
        env[variable] = str(path)
    subprocess.run(["bwrap", "--unshare-net", "--unshare-pid", "--die-with-parent", "--new-session",
        "--ro-bind", "/", "/", "--tmpfs", "/home", "--tmpfs", "/tmp", "--tmpfs", "/var/tmp",
        "--dev-bind", "/dev", "/dev", "--proc", "/proc",
        "--ro-bind", HERE.parents[1], HERE.parents[1], "--bind", evidence, evidence,
        "--chdir", evidence, sys.executable, Path(__file__).resolve(), "--bin-dir", binaries,
        "--evidence", evidence, "--inside-netns", os.readlink("/proc/self/ns/net")],
        env=env, check=True, timeout=480)


if __name__ == "__main__":
    main()
