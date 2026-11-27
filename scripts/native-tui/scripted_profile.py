"""Disposable HTTP provider and fresh profile for existing process fixtures."""
import importlib.util
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import threading

_path = Path(__file__).with_name("loopback-provider.py")
_spec = importlib.util.spec_from_file_location("profile_provider_transport", _path)
_transport = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_transport)


class ScriptedProfile:
    """Own a bounded provider until all clients of the fresh profile have exited."""
    def __init__(self, product, profile, environment, calls=()):
        if len(calls) > 32 or len(json.dumps(calls).encode()) > 65536:
            raise ValueError("fixture calls exceed their bound")
        self.product, self.profile, self.environment = product, profile, environment
        self.calls = list(calls)
        self.stop = threading.Event()
        self.failures = []
        self.listener = self.worker = None

    def __enter__(self):
        try:
            self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            self.listener.bind(("127.0.0.1", 0))
            self.listener.listen(8)
            self.listener.settimeout(0.1)
            endpoint = "http://127.0.0.1:" + str(self.listener.getsockname()[1])
            self.worker = threading.Thread(target=self.serve, daemon=True)
            self.worker.start()
            with tempfile.TemporaryDirectory(prefix="grapt-provider-inputs-") as directory:
                source = Path(directory)
                config = {"store": {"authority": "initialize"}, "product": {"build": "fixture"},
                          "provider": {"id": "openai", "model": "fixture-model", "base_model": "fixture-model",
                                       "family": "openai-responses", "protocol_family": "openai-responses",
                                       "endpoint": endpoint, "route": "responses", "account": "fixture-account",
                                       "credential_version": "fixture-v1", "policy_version": "fixture-policy", "variants": []}}
                (source / "grapt.jsonc").write_text(json.dumps(config), encoding="utf-8")
                (source / "account-models.json").write_text(json.dumps({"data": [{"id": "fixture-model", "max_model_len": 128000}]}), encoding="utf-8")
                (source / "provider.credential").write_text("SYNTHETIC_LOOPBACK_KEY", encoding="utf-8")
                result = subprocess.run([str(self.product), "init-profile", "--service", str(self.profile), "--from", str(source)],
                                        env=self.environment, capture_output=True, timeout=90, check=True)
                self.receipt = json.loads(result.stdout)
                if self.receipt.get("outcome") != "configured":
                    raise RuntimeError("fixture profile was not configured")
            return self
        except BaseException:
            self.close()
            raise

    def serve(self):
        while not self.stop.is_set():
            try:
                stream, _ = self.listener.accept()
            except TimeoutError:
                continue
            except OSError as error:
                if not self.stop.is_set():
                    self.failures.append(str(error))
                break
            try:
                with stream:
                    stream.settimeout(3)
                    _transport.read_request(stream, diagnostics=False)
                    if self.calls:
                        call = self.calls.pop(0)
                        frames = _transport.tool_call_frames(call["arguments"], 0, call["tool"])
                    else:
                        frames = [("data: " + json.dumps(value) + "\n\n", 0) for value in (
                            {"type": "response.output_text.delta", "delta": "Fixture complete."},
                            {"type": "response.output_text.done"},
                            {"type": "response.completed", "response": {"usage": {"input_tokens": 7, "output_tokens": 5, "total_tokens": 12}}},
                        )]
                    body = "".join(frame for frame, _ in frames).encode()
                    head = f"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {len(body)}\r\nConnection: close\r\n\r\n".encode()
                    stream.sendall(head + body)
                    stream.shutdown(socket.SHUT_WR)
            except (BrokenPipeError, ConnectionResetError):
                # A cancelled fixture client may close its response stream.
                pass
            except Exception as error:
                self.failures.append(str(error))
                self.stop.set()

    def close(self):
        self.stop.set()
        if self.listener is not None:
            self.listener.close()
        if self.worker is not None:
            self.worker.join(timeout=5)
            if self.worker.is_alive():
                raise RuntimeError("fixture provider failed to stop")
        if self.failures:
            raise RuntimeError("fixture provider failed: " + "; ".join(self.failures))

    def __exit__(self, *_exception):
        self.close()
