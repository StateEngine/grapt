"""Exercise one disposable product installation; retain its inputs and receipts."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import time
import threading


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(65536), b""):
            value.update(block)
    return value.hexdigest()


def domain(path):
    result = {}
    with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True) as connection:
        for table in ("event", "event_sequence", "kv", "project"):
            exists = connection.execute(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?", (table,)
            ).fetchone()[0]
            if not exists:
                result[table] = {"absent": True}
                continue
            rows = connection.execute(f'SELECT * FROM "{table}" ORDER BY rowid LIMIT 101').fetchall()
            if len(rows) > 100:
                raise RuntimeError("operator fixture exceeded its 100-row table bound")
            result[table] = {"absent": False, "rows": rows}
    return hashlib.sha256(json.dumps(result, sort_keys=True).encode()).hexdigest()


def initialized_receipt(exit_code: int, stdout: str | bytes, root: Path) -> dict:
    """Fresh Initialize must serve a Ready store and release its registration."""
    try:
        receipt = json.loads(stdout)
    except (ValueError, UnicodeDecodeError) as error:
        raise RuntimeError("fresh initialization returned no launch receipt") from error
    if (exit_code != 0 or not isinstance(receipt, dict)
            or receipt.get("schema") != "grapt.product.launch.v1"
            or receipt.get("outcome") != "served"
            or receipt.get("registration_released") is not True):
        raise RuntimeError("fresh initialization did not serve and release its registration")
    if not (root / "database.sqlite3").is_file():
        raise RuntimeError("fresh initialization left no Ready database")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--build", required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    root = args.root.absolute()
    root.mkdir(parents=False, exist_ok=False)
    environment = {key: os.environ[key] for key in ("PATH", "SystemRoot", "WINDIR", "TEMP", "TMP") if key in os.environ}
    environment.update({"HOME": str(root), "USERPROFILE": str(root), "GIT_CONFIG_NOSYSTEM": "1", "RUST_BACKTRACE": "0"})
    flags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    records = []

    def run(label, arguments, expected=0):
        child = subprocess.Popen(arguments, env=environment, cwd=root, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, creationflags=flags)
        captures = [bytearray(), bytearray()]
        failures = []

        def read_pipe(index, stream):
            try:
                while True:
                    block = stream.read(min(65536, 524289 - len(captures[index])))
                    if not block:
                        return
                    captures[index].extend(block)
                    if len(captures[index]) > 524288:
                        failures.append("output exceeds 512 KiB per stream")
                        child.kill()
                        return
            except OSError as error:
                failures.append(str(error))

        readers = [threading.Thread(target=read_pipe, args=(index, stream), daemon=True)
                   for index, stream in enumerate((child.stdout, child.stderr))]
        for reader in readers:
            reader.start()
        try:
            child.wait(timeout=20)
        finally:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=5)
            for reader in readers:
                reader.join(timeout=5)
        if any(reader.is_alive() for reader in readers):
            raise RuntimeError(f"{label}: output pipe did not close within its bound")
        child.stdout.close()
        child.stderr.close()
        output = subprocess.CompletedProcess(arguments, child.returncode, bytes(captures[0]), bytes(captures[1]))
        if failures:
            (root / f"{label}.stdout").write_bytes(output.stdout)
            (root / f"{label}.stderr").write_bytes(output.stderr)
            raise RuntimeError(f"{label}: {failures}; inspect retained bounded logs")
        (root / f"{label}.stdout").write_bytes(output.stdout)
        (root / f"{label}.stderr").write_bytes(output.stderr)
        records.append({"step": label, "exit": output.returncode})
        if output.returncode != expected:
            raise RuntimeError(f"{label}: exit {output.returncode}, expected {expected}; inspect retained logs")
        return output

    run("git-init", ["git", "init", "--quiet", str(root)])
    config = {
        "store": {"authority": "initialize"}, "product": {"build": args.build},
        "provider": {"id": "openai", "model": "gpt-5.6", "base_model": "gpt-5.6", "family": "gpt-5", "protocol_family": "openai-responses", "endpoint": "http://127.0.0.1:1",
                     "route": "responses", "account": "account-a", "credential_version": "credential-v1",
                     "policy_version": "df0-policy-v1", "variants": []},
    }
    (root / "account-models.json").write_text(json.dumps({"data": [{"id": "gpt-5.6", "max_model_len": 128000}]}), encoding="utf-8")
    service = "slot=product\ninstance=operator\nenvironment=internal-alpha\nversion=0.1.0\nserve_window_ms=2000\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=60000\nprovider_active_attempts=4\n"
    (root / "service.conf").write_text(service, encoding="utf-8")
    (root / "basic.credential").write_text("alpha:zephyr", encoding="utf-8")
    (root / "provider.credential").write_text("fixture-not-a-provider-secret", encoding="utf-8")

    def authority(value):
        config["store"]["authority"] = value
        (root / "grapt.jsonc").write_text(json.dumps(config), encoding="utf-8")

    serve = [str(binary), "serve", "--service", str(root), "--port", "0"]
    authority("initialize")
    initialized = run("initialize", serve)
    initialized_receipt(initialized.returncode, initialized.stdout, root)
    authority("open-ready")
    source = root / "database.sqlite3"
    before = domain(source)
    with sqlite3.connect(source.as_uri() + "?mode=ro", uri=True) as connection:
        with sqlite3.connect(root / "product-created-original.sqlite3") as backup:
            connection.backup(backup)
    (root / "serve-ready.json").unlink(missing_ok=True)
    with (root / "incumbent.stdout").open("wb") as stdout, (root / "incumbent.stderr").open("wb") as stderr:
        incumbent = subprocess.Popen(serve, env=environment, cwd=root, stdout=stdout, stderr=stderr, creationflags=flags)
    try:
        ready_path = root / "serve-ready.json"
        for _ in range(100):
            if ready_path.is_file():
                break
            if incumbent.poll() is not None:
                raise RuntimeError("incumbent exited before readiness")
            time.sleep(0.02)
        ready = json.loads(ready_path.read_text(encoding="utf-8"))
        if ready["pid"] != incumbent.pid:
            raise RuntimeError("ready receipt names another process")
        (root / "incumbent-ready.json").write_text(json.dumps(ready, indent=2), encoding="utf-8")
        # Poll/reap the incumbent concurrently: a zombie is still present in
        # /proc, while the product correctly requires observed process absence.
        reaper = threading.Thread(target=incumbent.wait)
        reaper.start()
        result = run("handover", [str(binary), "handover", "--service", str(root), "--run-root", str(root / "run"),
                                  "--run-id", "operator-handover", "--engine-version", args.build])
        reaper.join(timeout=5)
        if reaper.is_alive() or incumbent.returncode != 0:
            raise RuntimeError("incumbent did not retire within its bound")
        receipt = json.loads(result.stdout.splitlines()[-1])
        if not receipt["handover"]["retired_incumbent"] or not receipt["registration_released"]:
            raise RuntimeError("handover did not retire and release ownership")
        if receipt["service_instance"] == ready["service_instance"]:
            raise RuntimeError("successor reused the incumbent owner identity")
        if ready_path.exists() or (root / "registration").exists() or domain(source) != before:
            raise RuntimeError("handover left stale custody or changed fixture domain state")
        evidence = {"schema": "grapt.handover.operator.v1", "binary_sha256": digest(binary), "build": args.build,
                    "root": str(root), "source_origin": "product fresh initialize then open-ready", "domain": before,
                    "incumbent": ready, "successor": receipt, "steps": records, "children_reaped": True}
        (root / "operator-receipt.json").write_text(json.dumps(evidence, indent=2) + "\n", encoding="utf-8")
        print(json.dumps(evidence))
    finally:
        if incumbent.poll() is None:
            incumbent.kill()
        incumbent.wait(timeout=5)


if __name__ == "__main__":
    main()
