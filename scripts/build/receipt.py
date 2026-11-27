"""Bind a reusable gate result to its clean source, diff base and unchanged log."""
import argparse
import hashlib
import json
import re
from pathlib import Path
import subprocess
import time
import stat
import uuid
import test_features
import gate_plan
import slow_tests
import gate_retry
import gate_policy


def source():
    return {
        "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], timeout=10).decode().strip(),
        # Status can wait on this host's disk during a full gate. It is a read,
        # so do not refresh the index or leave an optional index.lock behind.
        "clean": not subprocess.check_output(["git", "--no-optional-locks", "status", "--porcelain"], timeout=60),
    }


def digest(path):
    return hashlib.sha256(gate_retry.read_log(path)).hexdigest()


RUN_HEADER = '# secode-gate-run '
# The gate receipt schema every producer writes and every reader checks.
SCHEMA = 'secode.gate.v1'
RUN_ID_LENGTH = 32


def begin_run(log):
    path = log.with_suffix(log.suffix + ".receipt.json")
    if path.is_file():
        path.unlink()
    return uuid.uuid4().hex


def log_run_binding(log):
    # (state, run id) for the log's first line: absent, ok, or malformed.
    with log.open('rb') as stream:
        line = stream.readline(len(RUN_HEADER) + RUN_ID_LENGTH + 1)
    if not line.startswith(RUN_HEADER.encode('ascii')):
        return 'absent', None
    if not line.endswith(b'\n') or len(line) != len(RUN_HEADER) + RUN_ID_LENGTH + 1:
        return 'malformed', None
    run_id = line.decode('ascii')[len(RUN_HEADER):-1]
    if not re.fullmatch(r'[0-9a-f]{32}', run_id):
        return 'malformed', None
    return 'ok', run_id


def write(log, mode, before, commands, seconds, wait_seconds=0, base=None, command_seconds=None, retries=None, test_tier="landing", retained_proof=None, native_windows=None, nextest_junit=None, batch=False):
    after = source()
    record = {"schema": SCHEMA, "mode": mode, "exit": 0,
              "source_before": before, "source_after": after,
              "commands": commands, "seconds": seconds,
              "wait_seconds": wait_seconds,
              "finished_unix": time.time(), "log_sha256": digest(log)}
    if retained_proof is not None:
        record['reviewed_composition'] = retained_proof
    if native_windows is not None:
        record["native_windows"] = native_windows
    if nextest_junit is not None:
        record["nextest_junit"] = nextest_junit
    record["test_tier"] = test_tier
    if batch:
        record['batch'] = True
    record['status'] = 'PASS_WITH_RETRY' if retries else 'PASS'
    if retries:
        record['retries'] = retries
    if command_seconds is not None:
        record["command_seconds"] = command_seconds
    if base is not None:
        record["base"] = base
    state, run_id = log_run_binding(log)
    if state != 'ok':
        raise ValueError('Gate receipt requires a bound run header')
    record['run'] = run_id
    encoded = json.dumps(record, indent=2) + "\n"
    if len(encoded.encode()) > 65536:
        raise ValueError('Gate receipt exceeds its 64 KiB bound')
    path = log.with_suffix(log.suffix + ".receipt.json")
    if mode == 'reviewed':
        with path.open('x', encoding='utf-8') as output:
            output.write(encoded)
    else:
        path.write_text(encoded)


def verify(log, commit, now=None, base=None, require_full=False, retained_full=None, batch=False):
    path = log.with_suffix(log.suffix + ".receipt.json")
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 65536:
        raise ValueError("Gate receipt must be a bounded regular file")
    record = json.loads(path.read_text())
    tier = record.get("test_tier", "landing")
    if (type(record.get('batch', False)) is not bool or record.get('batch', False) != batch
            or batch and (tier != 'landing' or record.get('mode') not in ('affected', 'full'))):
        raise ValueError('Gate receipt batch policy differs from the requested verification')
    if tier not in ("landing", "nightly") or (tier == "nightly" and record.get("mode") != "full"):
        raise ValueError("Invalid gate test tier")
    required = gate_plan.full_commands([], nightly=tier == "nightly")
    if record.get("native_windows") is not None:
        required = [command for command in required if command[:2] != ["cargo", "xwin"]]
    age = (time.time() if now is None else now) - record["finished_unix"]
    mode = record.get("mode")
    status = record.get('status','PASS')
    if (record.get("schema") != SCHEMA or mode not in ("full", "affected", "reviewed")
            or record.get("exit") != 0 or not 0 <= age <= 86400
            or record.get("source_before") != {"commit": commit, "clean": True}
            or record.get("source_after") != {"commit": commit, "clean": True}
            or status not in ('PASS','PASS_WITH_RETRY')
            or (status == 'PASS' and record.get('retries',[]) != [])
            or record.get("log_sha256") != digest(log)):
        raise ValueError("Gate receipt is stale, incomplete, changed, or belongs to another source")
    state, run_id = log_run_binding(log)
    if (state == 'malformed' or (state == 'absent') != ('run' not in record)
            or (state == 'ok' and record.get('run') != run_id)):
        raise ValueError("Gate receipt is stale, incomplete, changed, or belongs to another source")
    if any(command[:3]==['cargo','nextest','run'] for command in record.get('commands',[]) if isinstance(command,list)):
        import nextest_gate
        checksum=record.get('nextest_junit')
        if not isinstance(checksum,str) or len(checksum)!=64:
            raise ValueError('Nextest gate is missing its retained JUnit result')
        nextest_gate.verify(log,checksum)
    elif 'nextest_junit' in record:
        raise ValueError('Unexpected nextest result for a Cargo-only gate')
    if mode == "full":
        actual = record.get("commands", [])
        if any(not any(slow_tests.equivalent(recorded, command)
                       for recorded in actual if isinstance(recorded, list))
               for command in required):
            raise ValueError("Full gate receipt is missing required workspace commands")
        if base is None:
            raise ValueError('Full gate verification requires an explicit diff base for Windows coverage')
    if mode == 'reviewed':
        if require_full:
            raise ValueError(gate_policy.RETAINED_FULL_POLICY)
        if retained_full is None or base is None or record.get('base') != base:
            raise ValueError('Reviewed composition requires explicit retained full and exact base; it is not a full gate')
    elif retained_full is not None or 'reviewed_composition' in record:
        raise ValueError(gate_policy.RETAINED_FULL_POLICY)
    if mode == "affected":
        if base is None or record.get("base") != base:
            raise ValueError("Affected gate receipt base differs from current main or is missing")
        if require_full:
            raise ValueError("Selected gate policy requires a full workspace gate receipt")
    if base is not None:
        # Recompute current-main coverage for either reusable gate mode.
        subprocess.check_output(["git", "merge-base", "--is-ancestor", base, commit], timeout=10)
        paths = subprocess.check_output(
            ["git", "diff", "--no-renames", "--name-only", "-z", base, commit], timeout=60,
        ).decode().split("\0")
        root = Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], timeout=10).decode().strip())
        metadata = json.loads(subprocess.check_output(
            ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], timeout=60))
        members = set(metadata["workspace_members"])
        packages = [package for package in metadata["packages"] if package["id"] in members]
        changed = {path for path in paths if path}
        if mode == "affected" and gate_policy.required_mode(changed, root=root, base=base, head=commit) == "full":
            raise ValueError("Changed paths require a full workspace gate receipt")
        if mode == 'reviewed':
            import reviewed_composition
            proof = reviewed_composition.proof(root, commit, base, retained_full)
            if record.get('reviewed_composition') != proof:
                raise ValueError('Reviewed composition retained full proof changed')
            changed = set(proof['fresh_paths'])
            expected = reviewed_composition.commands(packages, changed, root, base)
        else:
            expected = gate_plan.commands(mode, packages, changed, root, base=base,
                                          nightly=tier == "nightly", batch=batch, head=commit)
        native_name = record.get('native_windows')
        scope = gate_plan.select(packages, gate_plan.scoped_paths(changed, root, base), root)
        if native_name is not None:
            if native_name != log.name+'.windows.json':
                raise ValueError('Native receipt must remain beside its owning gate log')
            import windows_gate
            windows_gate.verify_native(log.with_name(native_name), commit=commit, packages=scope,
                                       workspace=tier == 'nightly', nightly=tier == 'nightly')
        if native_name is not None:
            expected = [command for command in expected if command[:2] != ['cargo', 'xwin']]
        actual = record.get("commands", [])
        for check in expected:
            # The interpreter path may differ between login shells; its arguments may not.
            if check[0] != "cargo":
                present = any(command[1:] == check[1:] for command in actual if isinstance(command, list) and command and command[0])
            else:
                present = any(slow_tests.equivalent(command, check)
                              for command in actual if isinstance(command, list))
            if not present:
                raise ValueError(f"{mode.title()} gate receipt is missing required command: {check}")
    if status == 'PASS_WITH_RETRY':
        if base is None or record.get('base') != base:
            raise ValueError('PASS_WITH_RETRY requires the exact diff base')
        affected = gate_plan.select(packages, gate_plan.scoped_paths(changed, root, base), root)
        gate_retry.verify(record.get('retries'), record['commands'], log, packages, affected, root)
    elif gate_retry.retry_labels(gate_retry.read_log(log)):
        raise ValueError('A log with isolated retries requires PASS_WITH_RETRY evidence')
    return record


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("commit")
    parser.add_argument("--base", help="current controller main SHA; required for affected receipts")
    parser.add_argument("--require-full", action="store_true", help="require the full workspace gate even outside path exceptions")
    parser.add_argument('--batch', action='store_true', help='verify a landing-batch receipt and its exact Windows scope')
    parser.add_argument('--retained-commit')
    parser.add_argument('--retained-base')
    parser.add_argument('--retained-log')
    args = parser.parse_args()
    retained = (args.retained_commit, args.retained_base, args.retained_log)
    if any(retained) and not all(retained):
        parser.error('retained full requires commit/base/log together')
    retained = dict(zip(('commit', 'base', 'log'), retained)) if all(retained) else None
    record = verify(args.log, args.commit, base=args.base, require_full=args.require_full,
                    retained_full=retained, batch=args.batch)
    print(f"Fresh exact-source {record['mode']} {record.get('status','PASS')} receipt verified")
