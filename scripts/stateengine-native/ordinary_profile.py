"""Disposable ordinary TLS profile using the existing seat/service fixture owners."""
import hashlib
import json
import os
from pathlib import Path
import sys
import subprocess

from headless_acceptance import Journey, PACKET, write, service_helper
from acceptance_peer import CAPABILITY
from headless_acceptance import SESSION


class OrdinaryProfile(Journey):
    def __init__(self, args, endpoint, mcp_servers=None):
        self.args, self.mode = args, "ordinary-sustained"
        self.root = args.scratch / "journey"
        self.root.mkdir()
        self.profile = self.root / "profile"
        self.child, self.endpoint, self.logs = None, None, []
        self.environment = {"PATH": "/usr/bin:/bin", "TERM": "xterm-256color",
            "TOKIO_WORKER_THREADS": "2", "SSL_CERT_FILE": str(args.scratch / "tls/ca.crt")}
        if os.name == "nt":
            self.environment = {key: os.environ[key] for key in
                ("PATH", "SystemRoot", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP")
                if key in os.environ}
            self.environment.update(TERM="xterm-256color", TOKIO_WORKER_THREADS="2")
        if getattr(args, "loopback", False):
            self.environment["GRAPT_LIVE_HEADLESS_LOOPBACK"] = endpoint
        for key, name in [("GRAPT_TEST_HOME", "empty-home"), ("GRAPT_CONFIG_DIR", "empty-config")]:
            directory = args.scratch / name
            directory.mkdir()
            self.environment[key] = str(directory)
        inputs = self.root / "inputs"
        inputs.mkdir()
        write(inputs / "grapt.jsonc", {
            "store": {"authority": "initialize"}, "product": {"build": "ordinary-sustained"},
            "provider": {"id": "openai", "model": "gpt-live", "base_model": "gpt-live",
                "family": "gpt-5", "protocol_family": "openai-responses", "endpoint": endpoint,
                "route": "route-native", "account": "native-fixture", "credential_version": "fixture-v1",
                "policy_version": "fixture-v1", "variants": []},
            "catalog": {"source": "models.dev-openai"}, "compaction": {"server_side": False},
            "stateengine": {"native_mail_endpoint": "http://127.0.0.1:4877"},
            "permission": {"shell": "allow", "edit": "allow", "patch": "allow", "read": "allow", "stateengine_*": "allow", "elicitor_*": "allow"},
            "mcp": {**(mcp_servers or {}), "stateengine": {"type": "local", "codemode": False, "command": [
                sys.executable, str(Path(__file__).with_name("fixture_server.py")),
                "--mode", "seat-required-reads", "--calls-file", str(self.root / "mcp-calls.txt")]}}})
        (inputs / "provider.credential").write_text("disposable-model-dummy", encoding="utf-8")
        write(inputs / "model-catalog.json", {"openai": {"id": "openai", "name": "Fixture",
            "npm": "@ai-sdk/openai", "models": {"gpt-live": {"id": "gpt-live", "name": "gpt-live",
                "family": "gpt-5", "tool_call": True, "modalities": {"input": ["text"], "output": ["text"]},
                "limit": {"context": 128000, "output": 16384}}}}})
        write(inputs / "account-models.json", {"data": [{"id": "gpt-live"}]})
        if getattr(args, "loopback", False):
            config = json.loads((inputs / "grapt.jsonc").read_text())
            del config["provider"]["endpoint"]
            if getattr(args, "session_only", False):
                config["mcp"].pop("stateengine")
            write(inputs / "grapt.jsonc", config)
        original = dict(os.environ)
        try:
            os.environ.clear()
            os.environ.update(self.environment)
            service_helper.prepare(self.profile, inputs, args.product, 600000)
        finally:
            os.environ.clear()
            os.environ.update(original)
        # MCP launch commands belong to the disposable user's config. The
        # service directory is also this fixture's repository/workspace, so a
        # command left in its document is correctly refused as project-owned.
        document = self.profile / "grapt.jsonc"
        config = json.loads(document.read_text())
        mcp = config.pop("mcp", None)
        if mcp:
            write(document, config)
            write(Path(self.environment["GRAPT_CONFIG_DIR"]) / "grapt.json", {"mcp": mcp})
        self.packet = self.root / "packet.md"
        self.packet.write_bytes(PACKET)
        self.pointer = {"state": "written", "path": str(self.packet), "bytes": len(PACKET),
                        "sha256": hashlib.sha256(PACKET).hexdigest()}

    def provision(self):
        if not getattr(self.args, "session_only", False):
            return super().provision()
        created = self.api("POST", "/api/session", {"id": SESSION, "agent": "agt_live_headless1",
            "model": {"providerID": "openai", "id": "gpt-live"},
            "location": {"directory": str(self.profile)}})
        if created["data"]["id"] != SESSION:
            raise RuntimeError("session-only carrier received a different Session")
        return subprocess.CompletedProcess(["POST", "/api/session"], 0)

    def rebind(self):
        if not getattr(self.args, "session_only", False):
            self.packet.write_bytes(PACKET)
            self.api("PUT", f"/api/session/{SESSION}/stateengine/capability", {"capability": CAPABILITY})


def open_terminal(terminal, index):
    """Attach the real packaged TUI with only this disposable process environment."""
    from headless_acceptance import SESSION
    from tui_acceptance import command, prepared_catalog_ready, wait
    journey = terminal.journey
    terminal.session = command([terminal.args.driver, "start", "--seat", "seat-a", "--journey",
        f"sustained-{os.getpid()}-{index}", "--width", "160", "--height", "45", "--cwd", journey.profile,
        "--", "/usr/bin/env", "-i", *[f"{key}={value}" for key, value in journey.environment.items()],
        terminal.args.tui, "--service", journey.profile, "--session", SESSION,
        "--directory", journey.profile], journey.environment).strip()
    terminal.expect("enter send")
    wait(lambda: prepared_catalog_ready(terminal.drive("capture")), "ordinary catalog and saved choices")
