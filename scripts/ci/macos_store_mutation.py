"""Prove the native Store binding guard with three attributed resurrection failures."""

import argparse
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import sys
import time


ANCHOR = '''        #[cfg(target_os = "macos")]
        binding
            .validate(self.identity())
            .map_err(macos_binding::sqlite_error)?;'''
WITNESS = "mac_guard_rejects_swap_and_restore_during_"
EXPECTED = {WITNESS + route + "_open" for route in ("fresh", "existing", "migration")}
MAX_LOG_BYTES = 8 * 1024 * 1024


def check_result(output, code, failing):
    executed = re.findall(r"^test [\w:]*::(" + WITNESS + r"\w+) \.\.\.", output, re.M)
    failures = set(re.findall(r"^---- [\w:]*::(" + WITNESS + r"\w+) stdout ----$", output, re.M))
    if len(executed) != 3 or set(executed) != EXPECTED:
        raise RuntimeError("expected exactly the three native binding witnesses")
    if failing:
        if code == 0 or failures != EXPECTED:
            raise RuntimeError("mutant needs all three attributed binding failures")
    elif code != 0 or failures:
        raise RuntimeError("native binding witnesses failed")


def run_phase(root, command, log, failing, timeout=120):
    """Keep output and runtime bounded; terminate only this phase's process group."""
    with log.open("xb") as destination, selectors.DefaultSelector() as selector:
        process = subprocess.Popen(command, cwd=root, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        size = 0
        deadline = time.monotonic() + timeout
        try:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError("native Store phase timed out")
                for key, _ in selector.select(min(remaining, 0.2)):
                    chunk = os.read(key.fd, 65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    if size + len(chunk) > MAX_LOG_BYTES:
                        raise RuntimeError("native Store phase exceeded its log bound")
                    destination.write(chunk)
                    size += len(chunk)
            code = process.wait(timeout=max(0.01, deadline - time.monotonic()))
        finally:
            # Also reap descendants that closed stdout before their parent exited.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=10)
            process.stdout.close()
    check_result(log.read_text(encoding="utf-8", errors="replace"), code, failing)


def exercise(root, logs, cargo):
    path = root / "crates/grapt-store/src/migration/authority.rs"
    original = path.read_bytes()
    source = original.decode("utf-8")
    if source.count(ANCHOR) != 1:
        raise RuntimeError("native Store binding mutation target must occur exactly once")
    logs.mkdir(parents=True, exist_ok=True)
    command = cargo + ["test", "--locked", "-p", "grapt-store", "--lib", WITNESS,
                       "--no-fail-fast", "--", "--test-threads=1"]
    run_phase(root, command, logs / "store-binding-baseline.log", False)
    try:
        path.write_bytes(source.replace(ANCHOR, "").encode("utf-8"))
        run_phase(root, command, logs / "store-binding-mutant.log", True)
    finally:
        path.write_bytes(original)
    run_phase(root, command, logs / "store-binding-restored.log", False)
    print("MAC04_NATIVE_ABA_MUTANT_GREEN", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--logs", type=Path, required=True)
    parser.add_argument("cargo", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if sys.platform != "darwin":
        raise SystemExit("native Store binding evidence requires Darwin")
    command = args.cargo[1:] if args.cargo[:1] == ["--"] else args.cargo
    if not command:
        parser.error("supply the Cargo command after --")
    exercise(args.root.resolve(strict=True), args.logs.resolve(), command)


if __name__ == "__main__":
    main()
