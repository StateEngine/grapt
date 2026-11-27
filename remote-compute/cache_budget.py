#!/usr/bin/env python3
"""Read-only admission check for the two governed Linux cache carriers."""

from __future__ import annotations

import shlex
from pathlib import Path
import os
import argparse
import json
import re
import subprocess
import sys
import tempfile
from collections.abc import Callable
from typing import TypeAlias


sys.path.insert(0, str(Path(__file__).resolve().parents[1]/'scripts/build'))
import operator_paths

PROFILE_PATH = Path(os.environ.get('SECODE_COMPUTE_PROFILE_DIR', Path(__file__).parent))/'secode-linux.json'
with PROFILE_PATH.open('rb') as source:
    profile_bytes = source.read(1024 * 1024 + 1)
if len(profile_bytes) > 1024 * 1024:
    raise ValueError('Remote compute profile exceeds 1 MiB')
PROFILE = json.loads(profile_bytes)
SSH_TARGET = PROFILE['target']['ssh_target']
FILESYSTEM_ROOT = PROFILE['target']['remote_run_root'].rstrip('/') + '/cache'
CACHE_ROOTS = tuple(FILESYSTEM_ROOT + '/' + PROFILE['cache']['namespace'] + '/' +
                   PROFILE['resources']['domains'][domain]['cache_namespace'] + '/' + PROFILE['cache']['key']
                   for domain in ('domain0', 'domain1'))
MINIMUM_AVAILABLE_BYTES = 274_877_906_944
MAXIMUM_CARRIER_BYTES = 68_719_476_736
SSH_TIMEOUT_SECONDS = 30
# A transport hang must fail fast and be named as transport, well inside the 30 s bound.
SSH_CONNECT_TIMEOUT_SECONDS = 10
MAX_OUTPUT_BYTES = 4096
STDERR_TAIL_BYTES = 512
REFUSAL_EXIT = 125
ABSENT_SENTINEL = "ABSENT"
REMOTE_COMMAND = (
    "measure_carrier() { root=\"$1\"; "
    "if [ -L \"$root\" ]; then printf 'INVALID\\t%s\\n' \"$root\"; "
    "elif [ ! -e \"$root\" ]; then printf 'ABSENT\\t%s\\n' \"$root\"; "
    "elif [ ! -d \"$root\" ]; then printf 'INVALID\\t%s\\n' \"$root\"; "
    "else LC_ALL=C du -B1 -s -- \"$root\"; fi; }; "
    f"LC_ALL=C df -B1 --output=avail -- {shlex.quote(FILESYSTEM_ROOT)} && "
    f"measure_carrier {shlex.quote(CACHE_ROOTS[0])} && measure_carrier {shlex.quote(CACHE_ROOTS[1])}"
)
INTEGER = re.compile(r"[0-9]+")
Allocation: TypeAlias = int | None


# Every refusal names what the client actually observed. A timed-out measurement says which
# results had arrived, not why the rest did not: on 2026-09-26 the remote du finished in about
# 1 s while the Windows ssh client never delivered it, so "du did not complete" would be false.
KIND_CARRIER_RESULT_MISSING = "carrier-result-missing"
KIND_DF_RESULT_MISSING = "df-result-missing"
KIND_NO_OUTPUT = "no-output"
KIND_TRANSPORT = "transport"
KIND_SPACE = "space"
KIND_CARRIER_SIZE = "carrier-size"
KIND_MEASUREMENT = "measurement"


class BudgetRefusal(RuntimeError):
    """A measurement was unavailable, ambiguous, or outside the fixed budget."""

    def __init__(self, message: str, kind: str = KIND_MEASUREMENT) -> None:
        super().__init__(message)
        self.kind = kind


def _stderr_tail(stderr: bytes | str | None) -> str:
    if not stderr:
        return "no stderr"
    raw = stderr.encode("utf-8", errors="replace") if isinstance(stderr, str) else stderr
    tail = raw[-STDERR_TAIL_BYTES:].decode("utf-8", errors="replace").strip()
    return " | ".join(line for line in tail.splitlines() if line.strip()) or "no stderr"


def _incomplete(stdout: bytes | str | None, stderr: bytes | str | None) -> BudgetRefusal:
    """Name which results a timed-out measurement had delivered, claiming nothing about why."""
    raw = stdout.encode("utf-8", errors="replace") if isinstance(stdout, str) else (stdout or b"")
    lines = raw[:MAX_OUTPUT_BYTES].decode("utf-8", errors="replace").splitlines()
    bound = f"within {SSH_TIMEOUT_SECONDS} s"
    if not lines:
        return BudgetRefusal(
            f"read-only measurement produced no output {bound} (stderr: {_stderr_tail(stderr)})",
            KIND_NO_OUTPUT,
        )
    if lines[0].strip() != "Avail" or len(lines) < 2 or INTEGER.fullmatch(lines[1].strip()) is None:
        return BudgetRefusal(f"no filesystem df result was received {bound}", KIND_DF_RESULT_MISSING)
    available = lines[1].strip()
    completed = 0
    for expected_root, line in zip(CACHE_ROOTS, lines[2:]):
        if line.partition("\t")[2] != expected_root:
            break
        completed += 1
    if completed < len(CACHE_ROOTS):
        return BudgetRefusal(
            f"no carrier du result for {CACHE_ROOTS[completed]} was received {bound} "
            f"(df already measured {available} available bytes)",
            KIND_CARRIER_RESULT_MISSING,
        )
    return BudgetRefusal(f"every value was received but the measurement did not exit {bound}")


def _integer_bytes(value: str, field: str) -> int:
    stripped = value.strip()
    if INTEGER.fullmatch(stripped) is None:
        raise BudgetRefusal(f"{field} is not an exact integer byte count")
    parsed = int(stripped)
    if parsed > 9_223_372_036_854_775_807:
        raise BudgetRefusal(f"{field} exceeds the signed 64-bit measurement bound")
    return parsed


def parse_measurements(raw: bytes) -> tuple[int, tuple[Allocation, Allocation]]:
    if len(raw) > MAX_OUTPUT_BYTES:
        raise BudgetRefusal("measurement output exceeded its byte bound")
    try:
        lines = raw.decode("utf-8", errors="strict").splitlines()
    except UnicodeDecodeError as error:
        raise BudgetRefusal("measurement output is not UTF-8") from error
    if len(lines) != 4 or lines[0].strip() != "Avail":
        raise BudgetRefusal("measurement output is missing, malformed, or ambiguous")
    available = _integer_bytes(lines[1], "filesystem available bytes")
    allocations: list[Allocation] = []
    for index, expected_root in enumerate(CACHE_ROOTS, start=2):
        byte_text, separator, observed_root = lines[index].partition("\t")
        if separator != "\t" or observed_root != expected_root:
            raise BudgetRefusal("measurement named an unexpected carrier root")
        allocations.append(
            None
            if byte_text == ABSENT_SENTINEL
            else _integer_bytes(byte_text, f"{expected_root} allocated bytes")
        )
    return available, (allocations[0], allocations[1])


def _run_captured(argv: tuple[str, ...], *, timeout: float) -> subprocess.CompletedProcess[bytes]:
    """Run argv with stdin from DEVNULL and stdout/stderr captured to files, not pipes.

    Windows OpenSSH run from an ssh-spawned session with piped output never delivered
    EOF, although the remote command had finished in about 1 s (CACHE-BUDGET-ADMISSION-01:
    pipes and `-n -T` hung for 60 s, file capture exited in 2 s with every line). On the
    deadline ssh is killed and TimeoutExpired carries the partial output for naming.
    """
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=out, stderr=err)
        try:
            returncode = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            out.seek(0)
            err.seek(0)
            raise subprocess.TimeoutExpired(
                argv, timeout, output=out.read(MAX_OUTPUT_BYTES + 1), stderr=err.read(MAX_OUTPUT_BYTES + 1)
            ) from None
        out.seek(0)
        err.seek(0)
        return subprocess.CompletedProcess(
            argv, returncode, stdout=out.read(MAX_OUTPUT_BYTES + 1), stderr=err.read(MAX_OUTPUT_BYTES + 1)
        )


def measure(
    runner: Callable[..., subprocess.CompletedProcess[bytes]] = _run_captured,
) -> tuple[int, tuple[Allocation, Allocation]]:
    try:
        result = runner(
            (
                "ssh",
                "-o", "BatchMode=yes",
                "-o", f"ConnectTimeout={SSH_CONNECT_TIMEOUT_SECONDS}",
                "--", SSH_TARGET, REMOTE_COMMAND,
            ),
            timeout=SSH_TIMEOUT_SECONDS,
        )
    except OSError as error:
        raise BudgetRefusal(f"ssh could not be started: {error}", KIND_TRANSPORT) from error
    except subprocess.TimeoutExpired as error:
        raise _incomplete(error.stdout, error.stderr) from error
    # ssh reserves 255 for its own failures: connect timeout, auth or host-key refusal.
    if result.returncode == 255:
        raise BudgetRefusal(
            f"ssh transport to {SSH_TARGET} failed (exit 255): {_stderr_tail(result.stderr)}",
            KIND_TRANSPORT,
        )
    if len(result.stderr) > MAX_OUTPUT_BYTES:
        raise BudgetRefusal("measurement error output exceeded its byte bound")
    if result.returncode != 0:
        raise BudgetRefusal(f"read-only df/du measurement command returned {result.returncode}")
    if result.stderr:
        raise BudgetRefusal("read-only df/du measurement produced ambiguous error output")
    return parse_measurements(result.stdout)


def enforce(available: int, allocations: tuple[Allocation, Allocation]) -> None:
    if available < MINIMUM_AVAILABLE_BYTES:
        raise BudgetRefusal(
            f"filesystem available bytes {available} are below {MINIMUM_AVAILABLE_BYTES}",
            KIND_SPACE,
        )
    for root, allocated in zip(CACHE_ROOTS, allocations, strict=True):
        if allocated is not None and allocated > MAXIMUM_CARRIER_BYTES:
            raise BudgetRefusal(
                f"carrier {root} allocated bytes {allocated} exceed {MAXIMUM_CARRIER_BYTES}",
                KIND_CARRIER_SIZE,
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--phase", choices=("pre", "post"), required=True)
    args = parser.parse_args()
    try:
        available, allocations = measure()
        enforce(available, allocations)
    except BudgetRefusal as error:
        print(
            f"SECODE_CACHE_BUDGET_REFUSED phase={args.phase} kind={error.kind}: {error}",
            file=sys.stderr,
        )
        return REFUSAL_EXIT
    print(
        "SECODE_CACHE_BUDGET_EVIDENCE="
        + json.dumps(
            {
                "phase": args.phase,
                "ssh_target": SSH_TARGET,
                "filesystem_root": FILESYSTEM_ROOT,
                "available_bytes": available,
                "minimum_available_bytes": MINIMUM_AVAILABLE_BYTES,
                "carrier_allocated_bytes": dict(zip(CACHE_ROOTS, allocations, strict=True)),
                "maximum_carrier_bytes": MAXIMUM_CARRIER_BYTES,
                "action": "admit-read-only",
            },
            sort_keys=True,
            separators=(",", ":"),
        ),
        flush=True,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
