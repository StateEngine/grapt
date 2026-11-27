#!/usr/bin/env python3
"""Run provision/resume/mail acceptance against real CLI and testkit product binaries.

Uses only loopback model/MCP/mail fixtures and exclusively created profiles.
Never point this driver at a live profile or a release installation.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.error
import urllib.request

from acceptance_peer import AGENT, CAPABILITY, PRIVATE_CURSOR, PROJECT, Mail, Model, Server, load

SCRIPTS = Path(__file__).resolve().parents[1]
service_helper = load("seat_acceptance_service", SCRIPTS / "native-tui/service.py")
SESSION = "ses_native_seat_acceptance"
PACKET = b"packet_id: native_seat_packet\nSEAT_PACKET_TASK: continue admitted scratch work.\n"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def write(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def wait(read, description, seconds=20):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = read()
        if result:
            return result
        time.sleep(0.05)
    raise RuntimeError("did not observe " + description)


class Journey:
    def __init__(self, args, mode, model_endpoint):
        self.args, self.mode = args, mode
        self.root = args.scratch / mode
        self.root.mkdir()
        self.profile = self.root / "profile"
        self.child = None
        self.logs = []
        self.endpoint = None
        self.environment = {key: value for key, value in os.environ.items()
                            if not key.startswith(("SECODE_", "GRAPT_", "STATEENGINE_"))}  # grapt-rename: keep
        for key, name in [("GRAPT_TEST_HOME", "empty-home"), ("GRAPT_CONFIG_DIR", "empty-config")]:
            directory = self.root / name
            directory.mkdir()
            self.environment[key] = str(directory)
        self.environment["GRAPT_LIVE_HEADLESS_LOOPBACK"] = model_endpoint
        inputs = self.root / "inputs"
        inputs.mkdir()
        write(inputs / "grapt.jsonc", {
            "store": {"authority": "initialize"}, "product": {"build": "headless-seat-acceptance"},
            "provider": {"id": "openai", "model": "gpt-live", "base_model": "gpt-live",
                "family": "openai-responses", "route": "route-native", "account": "native-fixture",
                "credential_version": "fixture-v1", "policy_version": "fixture-v1", "variants": []},
            "catalog": {"source": "models.dev-chatgpt"}, "compaction": {"server_side": False},
            "stateengine": {"native_mail_endpoint": "http://127.0.0.1:4877"},
            "permission": {"stateengine_*": "allow"},
            "mcp": {"stateengine": {"type": "local", "codemode": False, "command": [
                sys.executable, str(SCRIPTS / "stateengine-native/fixture_server.py"),
                "--mode", "packet-bad-ack" if mode == "bad-ack" else "ready",
                "--calls-file", str(self.root / "mcp-calls.txt")]}}})
        (inputs / "provider.credential").write_text("disposable-model-dummy", encoding="utf-8")
        write(inputs / "model-catalog.json", {"openai": {"id": "openai", "name": "OpenAI",
            "npm": "@ai-sdk/openai", "models": {"gpt-live": {"id": "gpt-live", "name": "gpt-live",
                "family": "gpt-5", "tool_call": True, "modalities": {"input": ["text"], "output": ["text"]},
                "limit": {"output": 32000}}}}})
        write(inputs / "account-models.json", {"models": [{"slug": "gpt-live", "visibility": "list",
            "context_window": 128000, "supported_reasoning_levels": []}]})
        # The existing helper uses the process environment for initialization.
        # init only opens this disposable store and makes no model request.
        original = dict(os.environ)
        try:
            os.environ.clear()
            os.environ.update(self.environment)
            service_helper.prepare(self.profile, inputs, args.product, 300000)
        finally:
            os.environ.clear()
            os.environ.update(original)
        self.packet = self.root / "packet.md"
        self.packet.write_bytes(PACKET)
        self.pointer = {"state": "written", "path": str(self.packet), "bytes": len(PACKET),
                        "sha256": hashlib.sha256(PACKET).hexdigest()}
        if mode == "corrupt":
            self.packet.write_bytes(PACKET[:-2] + b"!\n")
        elif mode == "missing":
            self.packet.unlink()

    def cli(self, arguments, name, private_input=None):
        result = subprocess.run([str(self.args.cli), *arguments], cwd=self.profile,
            env=self.environment, input=private_input, capture_output=True, timeout=40)
        for suffix, value in [("stdout", result.stdout), ("stderr", result.stderr)]:
            require(len(value) <= 2 * 1024 * 1024, "CLI output exceeded evidence bound")
            require(CAPABILITY.encode() not in value and PRIVATE_CURSOR.encode() not in value,
                    "private binding reached CLI output")
            (self.root / f"{name}.{suffix}").write_bytes(value)
        return result

    def provision(self):
        identity = {"scope": "project", "project_id": PROJECT, "agent_id": AGENT,
                    "mail_transport_protocol": "remote-seat-mail-transport/v2"}
        return self.cli(["session", "provision", "--service", str(self.profile), "--session", SESSION,
            "--agent", "agt_live_headless1", "--model", "openai/gpt-live", "--location", str(self.profile),
            "--identity-entry", json.dumps(identity), "--packet-entry", json.dumps(self.pointer)],
            "provision", (CAPABILITY + "\n").encode())

    def start(self, index):
        out = (self.root / f"product-{index}.stdout").open("xb")
        err = (self.root / f"product-{index}.stderr").open("xb")
        self.logs.extend([out, err])
        self.child = subprocess.Popen([str(self.args.product), "serve-terminal", "--service",
            str(self.profile), "--port", "0"], cwd=self.profile, env=self.environment,
            stdin=subprocess.PIPE, stdout=out, stderr=err)

        def ready():
            require(self.child.poll() is None, "product exited before readiness")
            path = self.profile / "serve-ready.json"
            if not path.is_file():
                return None
            require(path.stat().st_size <= 8192, "readiness byte bound")
            try:
                value = json.loads(path.read_text())
            except json.JSONDecodeError:
                return None
            return value if value.get("pid") == self.child.pid else None

        self.endpoint = wait(ready, "matching product readiness")["observed_endpoint"]

    def stop(self):
        if self.child is None:
            return
        self.child.stdin.close()
        require(self.child.wait(timeout=15) == 0, "product failed to drain after control EOF")
        self.child = None

    def cleanup(self):
        if self.child is not None and self.child.poll() is None:
            self.child.kill()
            self.child.wait(timeout=5)
        for log in self.logs:
            log.close()

    def api(self, method, route, body=None):
        credential = f"{service_helper.USERNAME}:{service_helper.SECRET}"
        headers = {"Authorization": "Basic " + base64.b64encode(credential.encode()).decode(),
                   "Content-Type": "application/json"}
        request = urllib.request.Request(self.endpoint + route,
            data=None if body is None else json.dumps(body).encode(), headers=headers, method=method)
        with urllib.request.urlopen(request, timeout=5) as response:
            raw = response.read(2 * 1024 * 1024 + 1)
            require(len(raw) <= 2 * 1024 * 1024, "API evidence bound")
            require(CAPABILITY.encode() not in raw and PRIVATE_CURSOR.encode() not in raw,
                    "private binding reached public API")
            return json.loads(raw) if raw else None

    def run(self, name):
        return self.cli(["run", "--service", str(self.profile), "--session", SESSION,
                         "--prompt", "Continue the admitted scratch task."], name)

    def history(self):
        return self.api("GET", f"/api/session/{SESSION}/message?order=asc&limit=100")


def packet_case(args, mode):
    model, mail = Model(), Mail()
    with Server(0, model.dispatch) as provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        journey = Journey(args, mode, provider.endpoint)
        try:
            journey.start(0)
            provisioned = journey.provision()
            require(provisioned.returncode == 0, "provision failed; inspect provision.stderr")
            require(model.count() == 0, "provision made a provider call")
            first = journey.run("first")
            if mode in ("corrupt", "missing", "bad-ack"):
                require(first.returncode != 0, "invalid packet execution reported success")
                require(model.count() == 0, "packet refusal reached the provider")
                expected = {"corrupt": "pointer or packet bytes do not match", "missing": "launch packet is absent",
                            "bad-ack": "did not acknowledge this launch packet"}[mode]
                history = journey.history()
                require(expected in json.dumps(history), "packet refusal did not name its cause")
                write(journey.root / "history.json", history)
            else:
                require(first.returncode == 0, "valid packet run failed; inspect first.stderr")
                require(model.count() == 1, "first run did not make exactly one model request")
                journey.packet.unlink()
                require(journey.run("cached").returncode == 0, "same-process packet custody was lost")
                require(model.count() == 2, "cached packet run count differs")
                journey.stop()
                journey.start(1)
                require(journey.run("without-capability").returncode != 0,
                        "service restart silently retained a capability")
                journey.api("PUT", f"/api/session/{SESSION}/stateengine/capability", {"capability": CAPABILITY})
                require(journey.run("without-packet").returncode != 0,
                        "rebind resurrected absent packet bytes")
                require(model.count() == 2, "restart refusal reached the model")
                write(journey.root / "history.json", journey.history())
                journey.packet.write_bytes(PACKET)
                journey.api("PUT", f"/api/session/{SESSION}/stateengine/capability", {"capability": CAPABILITY})
                require(journey.run("redelivered").returncode == 0,
                        "supported rebind with redelivered packet did not recover")
                require(model.count() == 3, "recovered run did not reach the model exactly once")
                require(all("SEAT_PACKET_TASK" in json.dumps(body) for body in model.bodies),
                        "verified packet did not reach every model request")
            journey.stop()
            require(not provider.errors and not peer.errors, "fixture peer failed")
            result = {"case": mode, "passed": True, "model_requests": model.count(),
                      "billable_requests": 0, "product_drained": True}
            write(journey.root / "result.json", result)
            return result
        finally:
            journey.cleanup()
            write(journey.root / "peers.json", {"mail_operations": mail.operations,
                "mail_errors": peer.errors, "model_errors": provider.errors})
            write(journey.root / "model-requests.json", model.bodies)


def mail_case(args, startup=False):
    model, mail = Model(), Mail()
    with Server(0, model.dispatch) as provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        journey = Journey(args, "mail-startup" if startup else "mail", provider.endpoint)
        try:
            journey.start(0)
            require(journey.provision().returncode == 0, "mail Session provision failed")
            if not startup:
                require(journey.run("initial").returncode == 0, "initial headless run failed")
            initial = 0 if startup else 1
            require(model.count() == initial, "unexpected initial model request count")
            wait(lambda: mail.count("next_change"), "idle native mail subscription")
            mail.signal()
            wait(lambda: "ROW_A_MAIL_DONE" in json.dumps(journey.history()), "automatic mail read/reply")
            require(model.count() == initial + 3, "mail did not perform read, reply and final response exactly once")
            calls = (journey.root / "mcp-calls.txt").read_text().splitlines()
            require(calls.count("stateengine_msg_read") == calls.count("stateengine_msg_send") == 1,
                    "full mail read/reply did not execute exactly once")
            serialized = json.dumps(model.bodies)
            require("quiet_fixture" not in serialized, "quiet mail reached model input")
            require("FULL_BODY_END" in serialized and "msg_native_reply" in serialized,
                    "full mail body or reply receipt did not reach the model")
            require(all("SEAT_PACKET_TASK" in json.dumps(body) for body in model.bodies),
                    "automatic wake lost verified packet context")
            fetched, polled = mail.count("fetch_snapshot_page"), mail.count("next_change")
            mail.signal()
            wait(lambda: mail.count("fetch_snapshot_page") > fetched and mail.count("next_change") > polled,
                 "source rescan after repeated advisory")
            require(model.count() == initial + 3, "repeated advisory duplicated provider execution")
            write(journey.root / "history.json", journey.history())
            journey.stop()
            require(model.count() == initial + 3, "joined drain revealed duplicate provider execution")
            final_calls = (journey.root / "mcp-calls.txt").read_text().splitlines()
            require(final_calls.count("stateengine_msg_read") == final_calls.count("stateengine_msg_send") == 1,
                    "joined drain revealed duplicate mail read/reply")
            require(mail.count("close") == 1, "native mail subscription did not close once")
            require(sorted(mail.acks) == ["msg_native_long", "quiet_fixture"], "delivery ACKs differ from admitted rows")
            require(not provider.errors and not peer.errors, "fixture peer failed")
            result = {"case": journey.mode, "passed": True, "model_requests": model.count(),
                "idle_wake": True, "full_read_and_reply": True, "duplicate_advisory_suppressed": True,
                "mail_closed": True, "billable_requests": 0, "product_drained": True}
            write(journey.root / "result.json", result)
            return result
        finally:
            journey.cleanup()
            write(journey.root / "peers.json", {"mail_operations": mail.operations,
                "mail_errors": peer.errors, "model_errors": provider.errors})
            write(journey.root / "model-requests.json", model.bodies)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path, required=True)
    parser.add_argument("--product", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    cases = ["ready", "corrupt", "missing", "bad-ack", "mail", "mail-startup"]
    parser.add_argument("--case", choices=["all", *cases], default="all")
    args = parser.parse_args()
    args.cli, args.product = args.cli.resolve(strict=True), args.product.resolve(strict=True)
    version = subprocess.run([str(args.product), "--version"], capture_output=True, timeout=5, check=True)
    require(b"testkit/fixture: loopback-provider-testkit" in version.stdout,
            "requires an explicitly labelled testkit product; release binaries are refused")
    args.scratch = args.scratch.resolve()
    args.scratch.mkdir(mode=0o700)  # Exclusive: never overwrite an existing profile/evidence tree.
    write(args.scratch / "binaries.json", {"cli": str(args.cli), "product": str(args.product),
        "product_version": version.stdout.decode().strip(),
        "sha256": {str(path): digest(path) for path in (args.cli, args.product)}})
    results = []
    try:
        for case in (cases if args.case == "all" else [args.case]):
            result = mail_case(args, case == "mail-startup") if case.startswith("mail") else packet_case(args, case)
            results.append(result)
            print(json.dumps(result), flush=True)
    finally:
        write(args.scratch / "results.json", {"cases": results,
            "complete": len(results) == (len(cases) if args.case == "all" else 1),
            "real_stateengine_binding": "not measured", "release_package": "not measured",
            "seat_day": "not measured", "paid_provider_calls": 0})


if __name__ == "__main__":
    main()
