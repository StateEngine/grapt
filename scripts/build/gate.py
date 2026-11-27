#!/usr/bin/env python3
"""Run the delivery gate; Cargo owns dependency execution and artifact validity."""
import argparse
from contextlib import nullcontext
import fcntl
import json
import os
import signal
import stat
from pathlib import Path
import subprocess
import sys
import time
# A transported landing runner retains the qualified checkout's command policy.
# Only the runner needs to be newer to support exclusive full-slot admission.
if __name__ == '__main__' and '--source-policy' in sys.argv:
    os.environ['PYTHONDONTWRITEBYTECODE'] = '1'  # RAM supervisor children share the private helper tree.
    import importlib.util
    policy_parser = argparse.ArgumentParser(add_help=False)
    policy_parser.add_argument('--source-policy', type=Path, required=True)
    policy_options, _ = policy_parser.parse_known_args()
    policy_root = policy_options.source_policy.resolve()
    if policy_root != Path.cwd().resolve() or not (policy_root / '.git').exists():
        raise ValueError('Landing gate policy must belong to its current checkout')
    for policy_name in ('gate_scope', 'gate_policy', 'test_features', 'slow_tests', 'windows_gate', 'gate_plan'):
        policy_path = policy_root / 'scripts/build' / (policy_name + '.py')
        info = policy_path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_size > 65536:
            raise ValueError('Landing gate policy must be a bounded regular module')
        specification = importlib.util.spec_from_file_location(policy_name, policy_path)
        policy_module = importlib.util.module_from_spec(specification)
        sys.modules[policy_name] = policy_module
        specification.loader.exec_module(policy_module)

from git_environment import apply as apply_git_environment
apply_git_environment()

import receipt
import evict_targets
import signal_trace
import test_features
import ram_targets
import gate_retry
import gate_evidence
import gate_timing
import gate_scratch
import windows_gate
import reviewed_composition
import full_capacity
import gate_parallel
import nextest_gate

FULL_CAPACITY_PROTOCOL = 1


def output(*args):
    return subprocess.check_output(args, timeout=60)


def changed_paths(base):
    # Include committed, staged, unstaged, deleted, renamed and new files.
    paths = set()
    for args in [("diff", "--no-renames", "--name-only", "-z", base, "HEAD"),
                 ("diff", "--no-renames", "--name-only", "-z", "HEAD"),
                 ("ls-files", "--others", "--exclude-standard", "-z")]:
        paths.update(p.decode() for p in output("git", *args).split(b"\0") if p)
    return paths


# Retain the public helpers used by the gate tests and diagnostic callers.
from gate_plan import select, test_command, full_commands, affected_commands
import gate_plan
import publication_scan

FULL_LOCK = Path.home()/"secode-full-workspace.lock"
FULL_GATE_TIMEOUT_SECONDS = 10800
REVIEWED_GATE_TIMEOUT_SECONDS = 5400
NIGHTLY_PROTOCOL = 1
NIGHTLY_WAIT_PROTOCOL = 1


class GateSkipped(RuntimeError):
    """No gate commands ran because capacity or the measurement window refused."""


def full_holder():
    """Name the kernel lock owner without reading another seat's jobs or logs."""
    try:
        info = FULL_LOCK.stat()
        identity = (os.major(info.st_dev), os.minor(info.st_dev), info.st_ino)
        with Path('/proc/locks').open('rb') as stream:
            raw = stream.read(1024*1024+1)
        if len(raw) > 1024*1024:
            return 'holder unavailable: kernel lock table exceeds 1 MiB'
        owners = set()
        for line in raw.decode('ascii').splitlines():
            fields = line.split()
            if len(fields) != 8 or fields[1:3] != ['FLOCK', 'ADVISORY']:
                continue
            device = fields[5].split(':')
            if len(device) == 3 and (int(device[0], 16), int(device[1], 16), int(device[2])) == identity:
                owners.add(fields[4])
        return 'holder PID '+', '.join(sorted(owners)[:16]) if owners else 'holder not recorded'
    except (OSError, ValueError, UnicodeError) as error:
        return 'holder unavailable: '+type(error).__name__


def nightly_start_allowed():
    from datetime import datetime, timezone
    if datetime.now(timezone.utc).hour == 12:
        raise GateSkipped('measurement hold 12:00–13:00Z')


def full_writer_waiting(handle):
    """Honor writers already blocked on the host lock, including older helpers."""
    try:
        return full_capacity.writer_waiting(handle)
    except RuntimeError as error:
        raise GateSkipped(str(error)) from error


def full_slot(wait=0, *, exclusive=False):
    # Fixed across seats, cache overrides and StateEngine jobs on this host.
    try:
        import heavy
        claim = full_capacity.inherited(FULL_LOCK)
        shared = (not exclusive and (claim is not None or heavy.shared()
                  and (ram_targets.validate(Path(os.environ['CARGO_TARGET_DIR']), Path.cwd())
                       or heavy.nvme_target(os.environ['CARGO_TARGET_DIR']) is not None)))
        return full_capacity.acquire(FULL_LOCK, wait, shared=shared)
    except BlockingIOError:
        raise GateSkipped('slot busy ('+full_holder()+')') from None
    except subprocess.CalledProcessError as error:
        if error.returncode == 73:
            raise GateSkipped('slot busy ('+full_holder()+')') from None
        raise


def claim_target(target, root):
    # Resolve aliases: two symlinks to one directory must share an owner.
    target = target.resolve()
    target.mkdir(parents=True, exist_ok=True)
    with (target / ".secode-worktree").open("a+") as marker:
        fcntl.flock(marker, fcntl.LOCK_EX | fcntl.LOCK_NB)
        marker.seek(0)
        owner = marker.read()
        expected = str(root.resolve()) + "\n"
        if owner and owner != expected:
            raise RuntimeError(f"Target is owned by another checkout: {owner.strip()}")
        if not owner:
            marker.write(expected)
            marker.flush()


from heavy import stop_group


class GateTimeout(RuntimeError):
    """Execution exhausted its budget, independently of a failing command."""


def execute(command, log, deadline, cwd=None, lease_fd=None, observe=False, check=None):
    if (not isinstance(command, (list, tuple)) or not command
            or not isinstance(command[0], (str, os.PathLike))
            or not os.fsdecode(command[0]).strip()):
        raise ValueError('Gate command requires a non-empty executable')
    began = time.monotonic()
    remaining = deadline - began
    if remaining <= 0:
        raise GateTimeout("Gate timed out before the next command")
    admission = None
    try:
        import heavy
        descriptors = set(heavy.inherited_fds())
        if lease_fd is not None:
            descriptors.add(lease_fd)
        environment = windows_gate.environment(command)
        if command[0] == 'cargo':
            # A gate contains several Cargo invocations. Its initial lease
            # protects outputs, but cannot waive fresh capacity admission.
            target_environment = os.environ if environment is None else environment
            target = Path(target_environment['CARGO_TARGET_DIR'])
            if 'SECODE_TARGET_ROOT' not in target_environment:
                # An output outside the managed cache is not this cache's to
                # admit; running the whole gate there needs an explicit target.
                raise RuntimeError('A gate command needs the managed target root: '
                                   f'{target} is outside it and no SECODE_TARGET_ROOT names it')
            admission = evict_targets.prepare(Path(target_environment['SECODE_TARGET_ROOT']),
                                               target, Path(cwd or Path.cwd()),
                                               proc=Path('/proc'))
            descriptors.add(admission.fileno())
            if environment is not None:
                evict_targets.inherit(admission, target, environment, proc=Path('/proc'))
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise GateTimeout('Gate timed out during disk admission')
        options = {} if not descriptors else {'pass_fds': tuple(sorted(descriptors))}
        if environment is not None:
            options['env'] = environment
        if command[1:4] == ['-m', 'unittest', 'discover']:
            # Disposable checkouts exercise their own admission paths. Keep the
            # kernel lease and RAM scratch, but do not lend them this run's claims.
            options['env'] = {key: value for key, value in os.environ.items()
                              if key not in (heavy.CLAIM, full_capacity.CLAIM, 'SECODE_RAM_TARGET_RUN')}
        # The discovery is the longest-lived gate child, and the one a stray
        # signal has already killed with no record of its sender. Trace it, so
        # the next one names its sender; the reported command is unchanged.
        trace = command[1:4] == ['-m', 'unittest', 'discover']
        with nextest_gate.invocation(command,log) as invocation, \
                heavy.owned_process(signal_trace.wrapped(invocation) if trace else invocation,
                                    cwd=cwd, stdout=subprocess.PIPE if observe else log,
                                    stderr=subprocess.STDOUT, **options) as process:
            try:
                code = (gate_timing.wait(process, log, deadline, check=check) if observe else
                        gate_parallel.wait(process, deadline, check) if check is not None else
                        heavy.wait_owned(process, remaining))
            except subprocess.TimeoutExpired:
                raise GateTimeout("Gate timed out; its process group was stopped") from None
            except KeyboardInterrupt:
                raise RuntimeError("Gate interrupted; its process group was stopped") from None
        if code:
            raise subprocess.CalledProcessError(code, command)
    except BaseException as error:
        # owned_process has joined the group before its final status is exposed.
        if "process" in locals() and not hasattr(error, "returncode"):
            error.returncode = process.returncode
        raise
    finally:
        if "process" in locals() and process.stdout is not None:
            process.stdout.close()
        if admission is not None:
            admission.close()
        seconds = time.monotonic() - began
        log.write(f"# command_seconds: {seconds:.6f}\n")
        log.flush()
    return seconds


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["auto", "affected", "full", "composition", "reviewed"])
    parser.add_argument("--base", default="main", help="diff base (default main)")
    parser.add_argument("--list", action="store_true", help="show affected crates without running commands")
    parser.add_argument("--timeout", type=int,
                        help=f"overall execution limit in seconds (default full: {FULL_GATE_TIMEOUT_SECONDS}, affected: 1800)")
    parser.add_argument("--log", type=Path, help="command output file (default target/gate-logs/<pid>.log)")
    parser.add_argument('--wait', nargs='?', type=int, const=10800, default=0,
                        help='wait up to this many seconds for full capacity (default 3 hours)')
    parser.add_argument("--nightly", action="store_true", help="full gate including nightly-only slow tests")
    parser.add_argument('--native-windows', action='store_true',
                        help='explicitly request the native Windows sidecar')
    parser.add_argument("--row-a", action="store_true", help="Row A package qualification requires a full gate")
    parser.add_argument('--exclusive-full', action='store_true',
                        help='serialize full gates even with verified NVMe or RAM placement')
    parser.add_argument('--precheck', action='store_true',
                        help='pull request check: affected crates even when landing needs the full gate')
    parser.add_argument('--staging', action='store_true',
                        help='batch staging: cap precheck to whole touched crates and mandatory laws')
    parser.add_argument('--batch', action='store_true',
                        help='landing batch: retain Windows checks only for platform-sensitive changes')
    parser.add_argument('--fail-scratch-residue', action='store_true',
                        help='fail a passing gate that leaves test scratch residue in the test TMPDIR')
    parser.add_argument('--retained-commit')
    parser.add_argument('--retained-base')
    parser.add_argument('--retained-log')
    parser.add_argument('--source-policy', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if args.exclusive_full and args.mode not in ('full', 'auto'):
        parser.error('--exclusive-full requires full or auto mode')
    if args.precheck and (args.mode != 'affected' or args.row_a):
        parser.error('--precheck requires affected mode without Row A')
    if args.staging and not args.precheck:
        parser.error('--staging requires affected --precheck')
    if args.batch and (args.precheck or args.nightly or args.mode not in ('auto', 'full', 'affected')):
        parser.error('--batch requires a landing gate without precheck or nightly')
    retained = (args.retained_commit, args.retained_base, args.retained_log)
    if args.mode == 'reviewed':
        if not all(retained) or args.row_a or args.nightly or args.log is None:
            parser.error('reviewed requires explicit retained commit/base/log and its own log; no Row A/nightly')
        try:
            reviewed_composition.retained_arguments(dict(zip(('commit', 'base', 'log'), retained)))
        except ValueError as error:
            parser.error(str(error))
    elif any(retained):
        parser.error('retained full inputs require reviewed mode')
    if args.nightly and args.mode != "full":
        parser.error("--nightly requires full mode")
    if args.timeout is None:
        args.timeout = {"full": FULL_GATE_TIMEOUT_SECONDS, "affected": 1800, "composition": 900, "reviewed": REVIEWED_GATE_TIMEOUT_SECONDS, "auto": None}[args.mode]
    if args.timeout is not None and args.timeout <= 0:
        parser.error("--timeout must be positive")
    wait_limit = 14400 if args.nightly else 10800
    if args.wait < 0 or args.wait > wait_limit or (args.wait and args.mode not in ('full', 'auto')):
        parser.error(f'--wait is bounded to {wait_limit} seconds and applies only to full mode')
    if args.mode == 'composition' and not args.list and args.log is None:
        parser.error('composition requires its own explicit log path')
    return args


COMPOSITION_RESERVATION = 'SECODE_COMPOSITION_LOG_RESERVED'


def reserve_composition_log(options):
    # Only this supervisor's child may reuse the empty file it reserved. Consume
    # the claim before commands run so a nested composition reserves anew.
    if options.mode not in ('composition', 'reviewed') or options.list:
        return
    claim = os.environ.pop(COMPOSITION_RESERVATION, None)
    if (claim is not None and os.environ.get('SECODE_RAM_TARGET_RUN')
            and ram_targets.validate(Path(os.environ['CARGO_TARGET_DIR']), Path.cwd())):
        if len(claim) > 8192:
            raise ValueError('Oversized composition log reservation')
        info = options.log.lstat()
        expected = [str(options.log.resolve()), info.st_dev, info.st_ino]
        if (json.loads(claim) != expected or not stat.S_ISREG(info.st_mode)
                or info.st_size != 0):
            raise ValueError('Composition log reservation changed before its RAM child')
        return
    options.log.parent.mkdir(parents=True, exist_ok=True)
    with options.log.open('x') as reserved:
        info = os.fstat(reserved.fileno())
        os.environ[COMPOSITION_RESERVATION] = json.dumps(
            [str(options.log.resolve()), info.st_dev, info.st_ino])


def main():
    def interrupted(_signal, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    args = parse_args()
    run_id = None
    if not args.list:
        run_id = receipt.begin_run(args.log or Path(os.environ["CARGO_TARGET_DIR"]) / "gate-logs" / f"{os.getpid()}.log")
    started = time.monotonic()
    slot = None
    target_lease = None
    wait_seconds = 0
    log_path = args.log
    previous_base = os.environ.get('SECODE_GATE_BASE')
    try:
        root = Path(output("git", "rev-parse", "--show-toplevel").decode().strip())
        base = output("git", "rev-parse", "--verify", args.base + "^{commit}").decode().strip()
        os.environ['SECODE_GATE_BASE'] = base
        paths = changed_paths(base)
        if not args.list:
            violations = publication_scan.check(base)
            if violations:
                raise ValueError("\n".join(violations))
        retained_proof = None
        if args.mode == 'reviewed':
            exact = receipt.source()
            if not exact['clean']:
                raise ValueError('Reviewed composition requires a clean exact source')
            retained_proof = reviewed_composition.proof(root, exact['commit'], base,
                dict(commit=args.retained_commit, base=args.retained_base, log=args.retained_log))
            paths = set(retained_proof['fresh_paths'])
        required = gate_plan.required_mode(paths, row_a=args.row_a, root=root, base=base)
        if args.mode == "auto":
            args.mode = required
            args.timeout = args.timeout or (FULL_GATE_TIMEOUT_SECONDS if required == "full" else 1800)
        if args.row_a and args.mode != "full":
            raise ValueError("Row A package qualification requires a full gate")
        # A precheck never qualifies a landing: the landing batch runs the full gate.
        if args.mode == "affected" and required == "full" and not args.list and not args.precheck:
            raise ValueError("Changed paths require a full gate; use scripts/gate.sh auto or full")
        if args.mode == "full" and not args.list:
            if args.nightly:
                nightly_start_allowed()
            waiting = time.monotonic()
            if args.wait: print(f'Waiting for full gate capacity (limit {args.wait}s).', flush=True)
            slot = full_slot(args.wait, exclusive=args.exclusive_full)
            if args.nightly:
                nightly_start_allowed()
            wait_seconds = time.monotonic()-waiting
            print(f'Capacity wait: {wait_seconds:.1f}s', flush=True)
            started = time.monotonic()  # Queue time does not consume execution time.
        if 'SECODE_TARGET_ROOT' not in os.environ:
            raise RuntimeError('The gate needs the managed target root: set SECODE_TARGET_ROOT '
                               'by sourcing scripts/gate.sh, or pass a target inside the build cache')
        target_lease = evict_targets.prepare(Path(os.environ['SECODE_TARGET_ROOT']),
                                            Path(os.environ['CARGO_TARGET_DIR']), root,
                                            proc=Path('/proc'))
        evict_targets.inherit(target_lease, Path(os.environ['CARGO_TARGET_DIR']),
                              proc=Path('/proc'))
        claim_target(Path(os.environ["CARGO_TARGET_DIR"]), root)
        metadata = json.loads(output("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"))
        members = set(metadata["workspace_members"])
        packages = [p for p in metadata["packages"] if p["id"] in members]
        touched = (gate_plan.staging_select(packages, paths, root) if args.staging else
                   select(packages, gate_plan.scoped_paths(paths, root, base), root))
        if args.list:
            print("\n".join(touched))
            return 0
        before = receipt.source()
        if retained_proof is not None and before != exact:
            raise ValueError('Reviewed source changed before qualification')
        commands = (reviewed_composition.commands(packages, paths, root, base)
                    if args.mode == 'reviewed' else
                    gate_plan.commands(args.mode, packages, paths, root, base=base,
                                       nightly=args.nightly, staging=args.staging, batch=args.batch))
        for line in gate_plan.god_file_advisories(paths, root, base):
            print(line, flush=True)
        if (root/"crates/grapt-product/Cargo.toml").is_file():
            gate_plan.slow_tests.validate(root)
        if (args.mode in ("full", "composition", "reviewed") or touched) and not Path(os.environ["SECODE_FIXTURES"]).is_dir():
            raise RuntimeError("Frozen fixtures missing; set SECODE_FIXTURES or run scripts/build/install-fixtures.sh")
        log_path = args.log or Path(os.environ["CARGO_TARGET_DIR"]) / "gate-logs" / f"{os.getpid()}.log"
        log_path.parent.mkdir(parents=True, exist_ok=True)
        print(f"Log: {log_path}", flush=True)
        command_seconds = []
        retries = []
        with gate_parallel.native(log_path, commit=before.get("commit", ""), packages=touched if args.mode != "composition" else [],
                workspace=args.nightly, nightly=args.nightly, deadline=started+args.timeout,
                requested=args.native_windows) as native, \
                log_path.open("w", newline="\n") as log:
            log.write(f"# secode-gate-run {run_id}\n")
            log.flush()
            if native is not None:
                commands = [command for command in commands if command[:2] != ["cargo", "xwin"]]
            for index, command in enumerate(commands):
                print("+ " + " ".join(command), flush=True)
                log.write("+ " + " ".join(command) + "\n")
                log.flush()
                cwd = root
                seconds, retried = gate_retry.run(command, log, started + args.timeout,
                    execute=(lambda command, log, deadline, **kw: execute(command, log, deadline,
                        observe=command[:2] == ["cargo", "test"] and "--test-threads=1" in command,
                        check=native.check if native else None, **kw)),
                    packages=packages, affected=touched, index=index, cwd=cwd,
                    retry_budget=gate_retry.MAX_TESTS-sum(len(r['retries']) for r in retries),
                    lease_fd=target_lease.fileno() if target_lease else None)
                command_seconds.append(seconds)
                if retried is not None:
                    retries.append(retried)
        if target_lease is not None:
            evict_targets.retain(Path(os.environ["CARGO_TARGET_DIR"]), root)
        if retries:
            gate_retry.verify(retries, commands, log_path, packages, touched, root)
        # Composition has its own proof binding the original qualification and
        # both bases. It must never masquerade as an affected/full gate receipt.
        if retained_proof is not None:
            if (receipt.source() != before or
                    reviewed_composition.proof(root, before['commit'], base,
                        retained_proof['retained_full']) != retained_proof):
                raise ValueError('Reviewed source or retained full changed during qualification')
        # A passing run must not grow the test TMPDIR, and a refused run must
        # leave no reusable success evidence: the check precedes receipt
        # publication and the PASS line.
        residue_status = gate_scratch.residue(gate_scratch.active,
                                              require_clean=args.fail_scratch_residue)
        if residue_status != 0:
            return residue_status
        # The headless judge uses the base the gate resolved and published;
        # it never re-derives one from the environment.
        headless_status = gate_scratch.judge_headless_residue(
            gate_scratch.active_headless_base, require_clean=True)
        if headless_status != 0:
            return headless_status
        # A precheck leaves no receipt, so nothing can present it as landing evidence.
        if args.mode != "composition" and not args.precheck:
            receipt.write(log_path, args.mode, before, commands, time.monotonic()-started, wait_seconds,
                          base=base, command_seconds=command_seconds, retries=retries,
                          test_tier="nightly" if args.nightly else "landing", retained_proof=retained_proof,
                          batch=args.batch,
                          native_windows=native.receipt.name if native else None,
                          nextest_junit=nextest_gate.verify(log_path) if any(c[:3]==["cargo","nextest","run"] for c in commands) else None)
        status = 'PASS_WITH_RETRY' if retries else 'PASS'
        label = 'precheck (not landing evidence)' if args.precheck else args.mode
        print(f"{status} {label}: {len(touched)} affected crates, {time.monotonic()-started:.1f}s", flush=True)
        return 0
    except GateSkipped as error:
        if args.nightly:
            print(f'SKIPPED full: {error}', file=sys.stderr)
            return 74
        print(f'FAIL {args.mode}: {error}', file=sys.stderr)
        return 1
    except (evict_targets.DiskAdmissionRefused, gate_parallel.CapacityUnavailable) as error:
        if target_lease is not None and log_path is not None and not os.environ.get('SECODE_RAM_TARGET_RUN'):
            gate_evidence.preserve(Path(os.environ['CARGO_TARGET_DIR']), log_path)
            gate_evidence.archive_failure(log_path)
        print(error, file=sys.stderr)
        return 75
    except (subprocess.SubprocessError, OSError, ValueError, RuntimeError, KeyboardInterrupt) as error:
        if target_lease is not None and log_path is not None and not os.environ.get('SECODE_RAM_TARGET_RUN'):
            gate_evidence.preserve(Path(os.environ['CARGO_TARGET_DIR']), log_path)
            gate_evidence.archive_failure(log_path)
        timed_out = isinstance(error, GateTimeout)
        detail = f"Gate timed out at {args.timeout} s; its process group was stopped" if timed_out else str(error)
        print(f"FAIL {args.mode}: {detail} ({time.monotonic()-started:.1f}s)", file=sys.stderr)
        return 124 if timed_out else 1
    finally:
        if target_lease is not None:
            try:
                (Path(os.environ['CARGO_TARGET_DIR']) / '.secode-worktree').touch()
            finally:
                target_lease.close()
        if slot is not None:
            slot.close()
        if previous_base is None:
            os.environ.pop('SECODE_GATE_BASE', None)
        else:
            os.environ['SECODE_GATE_BASE'] = previous_base


def reserve_full_capacity(options):
    if options.list or options.mode not in ('full', 'auto'):
        return nullcontext()
    if options.mode == 'auto' and gate_plan.required_mode(
            changed_paths(options.base), row_a=options.row_a, root=Path.cwd(), base=options.base) != 'full':
        return nullcontext()
    import heavy
    if heavy.inherited() is not None and full_capacity.inherited() is None:
        raise ValueError('Full gates must acquire workspace capacity before their heavy lease; relaunch with the coherent heavy runner')
    shared = not options.exclusive_full and heavy.nvme_target(os.environ['CARGO_TARGET_DIR']) is not None
    return full_capacity.reserve(options.wait, shared=shared, path=FULL_LOCK)


if __name__ == "__main__":
    # The supervisor stays outside the capped scope, so an OOM cannot prevent cleanup.
    def interrupt_supervisor(_signal, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupt_supervisor)
    try:
        import cpu_priority
        options = parse_args()
        import durable_jobs
        import heavy
        if (not options.list and not durable_jobs.active() and heavy.inherited() is None
                and full_capacity.inherited() is None and not os.environ.get('SECODE_RAM_TARGET_RUN')):
            raise SystemExit(durable_jobs.run(
                [sys.executable, '-B', str(Path(__file__).resolve()), *sys.argv[1:]], heavy.owner()))
        with reserve_full_capacity(options):
            if not options.list and cpu_priority.configured() and not cpu_priority.active():
                raise SystemExit(cpu_priority.run(
                    [sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]],
                    timeout=min(43200, options.wait + (options.timeout or FULL_GATE_TIMEOUT_SECONDS) + 100)))
            with gate_scratch.scratch():
                options = parse_args()
                reserve_composition_log(options)
                ram_status = ram_targets.run_gate(sys.argv[1:], options.log)
                os.environ.pop(COMPOSITION_RESERVATION, None)
                # The suite process (disk here, RAM in the child) checks its
                # own scratch before publishing success evidence.
                status = main() if ram_status is None else ram_status
                sys.exit(status)
    except evict_targets.DiskAdmissionRefused as error:
        print(error, file=sys.stderr)
        sys.exit(75)
    except gate_scratch.HeadlessBaseRefused as error:
        print(error, file=sys.stderr)
        sys.exit(75)
    except KeyboardInterrupt:
        sys.exit(130)
