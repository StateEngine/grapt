#!/usr/bin/env python3
"""Run main's nightly gate and safely revert a reproducible, attributed regression."""
from operator_paths import data_root
import argparse
from datetime import datetime, timezone
import os
import re
import json
from pathlib import Path
import signal
import shlex
import subprocess
import sys
import tempfile
import time

import gate
import heavy
import nightly_failure
import full_capacity
from nightly_revert import Check, Publisher


GOOD_REF = 'refs/secode/nightly-green'
MAX_COMMITS = 256
FULL_TIMEOUT_SECONDS = 10800
FULL_SLOT_WAIT_SECONDS = 4*3600
ATTEMPT_OVERHEAD_SECONDS = 150  # Child shutdown margin plus receipt verification.
AUDIT_TIMEOUT_SECONDS = 12*3600
MAX_LOG_GROUPS = 16
MAX_NAMED_FAILURES = 16
MAX_OUTPUT_BYTES = 512 * 1024**2
MAX_LOG_BYTES = 64 * 1024**2
STATE = data_root()/'logs/nightly'
_STOP_REQUESTED = False
_STOP_SIGNAL = None


class NightlySkipped(RuntimeError):
    """Capacity and measurement skips never certify or blame source."""


def request_stop(_signal, _frame):
    global _STOP_REQUESTED, _STOP_SIGNAL
    _STOP_REQUESTED = True
    _STOP_SIGNAL = _signal
    raise KeyboardInterrupt


def git(root, *arguments):
    return subprocess.check_output(['git', *arguments], cwd=root, timeout=120).decode().strip()


def save_result(state, result):
    state.mkdir(parents=True, exist_ok=True)
    temporary = state/'last-result.new'
    stamp = datetime.now(timezone.utc).isoformat()
    temporary.write_text(f'{stamp} {result}\n')
    temporary.replace(state/'last-result')


def report_blocker(publish, result, failures, dry_run):
    report = getattr(publish, 'report', None)
    if not dry_run and report is not None:
        try:
            report(result, failures)
        except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
            print(f'Nightly blocker notification failed: {error}', flush=True)


def retain_logs(state, current):
    """Bound only this controller's regular outputs; preserve unrelated entries."""
    groups = {}
    pattern = r'(\d{8}T\d{6}Z-[0-9a-f]{12})(?:\.log(?:\.receipt\.json|\.deps\.json|\.nextest\.(?:xml|toml)|\.windows\.(?:json(?:\.log)?|driver\.log))?|\.driver\.log)'
    with os.scandir(state) as entries:
        for index, entry in enumerate(entries):
            if index >= 4096:
                raise RuntimeError('Nightly output inventory exceeds 4096 entries')
            match = re.fullmatch(pattern, entry.name)
            if not match or not entry.is_file(follow_symlinks=False):
                continue
            groups.setdefault(match[1], []).append(Path(entry.path))
    order = sorted(groups, key=lambda name: (name == current, name), reverse=True)
    size = 0
    for index, name in enumerate(order):
        files = groups[name]
        amount = sum(min(path.stat().st_size, MAX_LOG_BYTES) for path in files)
        if name != current and (index >= MAX_LOG_GROUPS or size+amount > MAX_OUTPUT_BYTES):
            for path in files:
                path.unlink()
        else:
            size += amount
            for path in files:
                if path.stat().st_size > MAX_LOG_BYTES:
                    # Preserve a failed subject's diagnostic tail visibly,
                    # without copying oversized groups that will be deleted.
                    notice = b'[nightly: earlier output truncated at retention limit]\n'
                    with path.open('rb') as stream:
                        stream.seek(-(MAX_LOG_BYTES-len(notice)), os.SEEK_END)
                        tail = stream.read(MAX_LOG_BYTES-len(notice))
                    with path.open('wb') as stream:
                        stream.write(notice); stream.write(tail)
    if size > MAX_OUTPUT_BYTES:
        raise RuntimeError('Current nightly output exceeds retention capacity')


def source_failures(raw, driver):
    """Read named failures from child output, commands only from the supervisor."""
    tests = re.findall(r'^test ([^\r\n]{1,1024}) \.\.\. FAILED$', raw, re.M)
    tests += ['nextest: '+name for name in re.findall(r'^# nextest failure: ([^\r\n]{1,2048})$',raw,re.M)]
    tests += ['unittest: '+name for name in re.findall(
        r'^(?:FAIL|ERROR): [^\r\n]{1,512} \(([\w.]{1,512})\)(?: [^\r\n]{0,512})?$', raw, re.M)]
    if tests:
        return tuple(sorted(set(tests)))[:32]
    commands = []
    for line in re.findall(r'^\+ ([^\r\n]{1,2048})$', driver, re.M):
        try:
            arguments = shlex.split(line)
        except ValueError:
            continue
        if arguments and arguments[0].strip() and arguments[0] not in ('[]', '()'):
            commands.append(line)
    return ('gate command: '+commands[-1],) if commands else ()


def full_gate(root, state, commit, *, deadline=None):
    def wait_budget(maximum):
        if deadline is None:
            return maximum
        remaining = deadline-time.monotonic()-FULL_TIMEOUT_SECONDS-ATTEMPT_OVERHEAD_SECONDS
        if remaining <= 0:
            raise RuntimeError('Nightly diagnosis exceeded its twelve-hour bound')
        return min(maximum, remaining)
    stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    group = f'{stamp}-{commit[:12]}'
    log = state / f'{group}.log'
    gate_completed = False
    driver_log = log.with_suffix(".driver.log")
    temporary = state/"current-attempt.json.new"
    temporary.write_text(json.dumps(dict(commit=commit, logs=[str(driver_log), str(log)]))+"\n")
    temporary.replace(state/"current-attempt.json")
    retain_logs(state, group)
    with tempfile.TemporaryDirectory(prefix='secode-nightly-', dir='/tmp') as scratch:
        previous = os.environ.get('TMPDIR')
        previous_target = os.environ.get('CARGO_TARGET_DIR')
        os.environ['TMPDIR'] = scratch
        os.environ.pop('CC_ENABLE_DEBUG_OUTPUT', None)
        os.environ.pop('CARGO_TARGET_DIR', None)
        try:
            with log.with_suffix('.driver.log').open('w') as output:
                arguments = ['bash', 'scripts/gate.sh', 'full', '--timeout', str(FULL_TIMEOUT_SECONDS),
                             '--base', commit, '--log', str(log)]
                policy = root/'scripts/build/gate.py'
                policy_text = policy.read_text() if policy.is_file() and policy.stat().st_size <= 65536 else ''
                if (policy.is_file() and policy.stat().st_size <= 65536 and
                        'NIGHTLY_PROTOCOL = 1' in policy_text):
                    arguments.append('--nightly')
                # Old bisect subjects retain their immediate capacity policy;
                # only the explicit protocol promises the post-wait hold guard.
                waited_policy = 'NIGHTLY_WAIT_PROTOCOL = 1' in policy_text
                # Own each attempt separately. Publication subsequently enters
                # the ordinary landing queue, after this gate lease is released.
                target = heavy.nvme_target(Path.home()/'.cache/secode/target')
                gate.nightly_start_allowed()
                wait_seconds = int(wait_budget(FULL_SLOT_WAIT_SECONDS if waited_policy else 0))
                if 'FULL_CAPACITY_PROTOCOL = 1' not in policy_text:
                    arguments = [sys.executable, '-B', str(Path(gate.__file__).resolve()),
                                 'full', '--source-policy', str(root), *arguments[3:]]
                # Workspace waiters hold no heavy lease or future-memory charge.
                with full_capacity.reserve(wait_seconds, shared=target is not None):
                    gate.nightly_start_allowed()
                    heavy_wait = wait_budget(25200)
                    with heavy.slot('gate', wait=heavy_wait, job_owner='nightly', nvme=target) as lease:
                        gate.nightly_start_allowed()
                        gate.execute(arguments, output, time.monotonic()+FULL_TIMEOUT_SECONDS+60,
                                     cwd=root, lease_fd=lease.fileno())
                gate_completed = True
            record = log.with_suffix(log.suffix+'.receipt.json')
            if not record.is_file() or record.is_symlink() or record.stat().st_size > 65536:
                raise ValueError('Full gate did not produce a bounded regular receipt')
            if json.loads(record.read_text()).get('mode') != 'full':
                raise ValueError('Nightly requires a full gate receipt')
            # Verify using the subject revision's policy: feature matrices may
            # legitimately evolve after this supervisor was installed.
            with log.with_suffix('.driver.log').open('a') as output:
                gate.execute([sys.executable, 'scripts/build/receipt.py', str(log), commit,
                              '--base', commit], output, time.monotonic()+90, cwd=root)
            return Check(True, log)
        except (subprocess.SubprocessError, RuntimeError, ValueError, OSError, KeyboardInterrupt) as error:
            if isinstance(error, gate.GateSkipped):
                raise NightlySkipped(str(error)) from None
            if isinstance(error, subprocess.CalledProcessError) and error.returncode == 74:
                detail = nightly_failure.tail(driver_log)
                skipped = re.findall(r'^SKIPPED full: (slot busy \([^\r\n]{1,1024}\)|measurement hold 12:00–13:00Z)$', detail, re.M)
                if len(skipped) == 1:
                    raise NightlySkipped(skipped[0]) from None
            detail = nightly_failure.record(state, error, commit=commit,
                returncode=getattr(error, "returncode", None),
                requested_signal=_STOP_SIGNAL, logs=(driver_log, log))
            error.nightly_diagnostic = detail
            if _STOP_REQUESTED:
                interrupted = KeyboardInterrupt()
                interrupted.nightly_diagnostic = detail
                raise interrupted from error
            print(f'Full gate failed at {commit}: {detail}', flush=True)
            # Timeouts, capacity refusals and invalid receipts are not source
            # attribution. A failed gate command must leave diagnostic output.
            failures = ()
            if not gate_completed and isinstance(error, subprocess.CalledProcessError) and error.returncode == 1 and log.is_file():
                import gate_retry
                raw = gate_retry.read_log(log).decode('utf-8')
                driver = gate_retry.read_log(log.with_suffix('.driver.log')).decode('utf-8')
                failures = source_failures(raw, driver)
            return Check(False, log, failures[:32], detail)
        finally:
            try:
                retain_logs(state, group)
            finally:
                if previous is None:
                    os.environ.pop('TMPDIR', None)
                else:
                    os.environ['TMPDIR'] = previous
                if previous_target is None:
                    os.environ.pop('CARGO_TARGET_DIR', None)
                else:
                    os.environ['CARGO_TARGET_DIR'] = previous_target


def tested_revert(root, state, candidate, culprit, check):
    git(root, 'checkout', '--quiet', '--detach', candidate)
    parents = git(root, 'rev-list', '--parents', '-n', '1', culprit).split()
    command = ['git', 'revert', '--no-commit']
    if len(parents) > 2:
        command += ['--mainline', '1']
    reverted = subprocess.run([*command, culprit], cwd=root, capture_output=True, text=True, timeout=60)
    if reverted.returncode:
        if (root/'.git/REVERT_HEAD').exists() or (root/'.git/sequencer').exists():
            git(root, 'revert', '--abort')
        return None
    paths = subprocess.check_output(['git', 'diff', '--cached', '--name-only', '-z'],
                                    cwd=root, timeout=30).decode().split('\0')[:-1]
    if not paths or len(paths) > 4096 or sum(map(len, paths)) > 262144:
        raise RuntimeError('Nightly revert has no bounded changed-path list')
    message = (f'chore(nightly): revert {culprit}\n\n'
               'Nightly reproduced the regression at this commit and verified its parent.\n'
               'Co-Authored-By: secode-seats <353560407+secode-seats[bot]@users.noreply.github.com>\n')
    identity = []
    for field, format_code in (('name', '%an'), ('email', '%ae')):
        selected = gate.publication_scan.project_identity(field, root)
        if not selected:
            selected = git(root, 'log', '-1', '--format=' + format_code, candidate)
        identity += ['-c', 'user.' + field + '=' + selected]
    subprocess.run(['git', *identity, '-c', 'commit.gpgSign=false', 'commit', '-F', '-', '--', *paths],
                   input=message, text=True, cwd=root, check=True, capture_output=True, timeout=60)
    commit = git(root, 'rev-parse', 'HEAD')
    outcome = check(root, state, commit)
    return (commit, outcome) if outcome else None


def verdict_reason(reason, failures):
    """Name the failures an inconclusive verdict could not attribute.

    A bare INCONCLUSIVE once hid the same build-tooling failure for three
    nights. Naming the tests is not attribution: a failure that recurs in a
    baseline cannot be pinned on one commit, but an operator still has to see
    which tests it was. `source_failures` already caps its own list, so the
    count line below is reached only by a caller that hands over more.
    """
    if not failures:
        return reason
    if len(failures) > MAX_NAMED_FAILURES:
        return (reason+'; failing: '+', '.join(failures[:MAX_NAMED_FAILURES])+
                f' and {len(failures)-MAX_NAMED_FAILURES} more')
    return reason+'; failing: '+', '.join(failures)


def audit(root, state, candidate, check, *, dry_run=True, publish=None, deadline=None):
    """Bisect first-parent history using real green/red full-gate results."""
    save_result(state, f'RUNNING main {candidate}')
    deadline = time.monotonic()+AUDIT_TIMEOUT_SECONDS if deadline is None else deadline
    restore = candidate
    def bounded_check(*arguments):
        if time.monotonic()+FULL_TIMEOUT_SECONDS+ATTEMPT_OVERHEAD_SECONDS > deadline:
            raise RuntimeError("Nightly diagnosis exceeded its twelve-hour bound")
        value = check(*arguments)
        # Pure disposable-repository checks can use bool; the production
        # publisher additionally requires a real exact-source gate log.
        return value if isinstance(value, Check) else Check(bool(value), failures=() if value else ('fixture gate',))
    def run(commit):
        git(root, 'checkout', '--quiet', '--detach', commit)
        return bounded_check(root, state, commit)
    try:
        initial = run(candidate)
        if initial:
            if not dry_run:
                git(root, 'update-ref', GOOD_REF, candidate)
            result = f'PASS main {candidate}'
            code = 0
        else:
            known = subprocess.run(['git', 'rev-parse', '--verify', GOOD_REF], cwd=root,
                                   capture_output=True, text=True, timeout=10)
            if known.returncode:
                result = ('INCONCLUSIVE main '+candidate+': '+
                          verdict_reason('no last green nightly; no revert authorized',
                                         initial.failures))
                save_result(state, result)
                report_blocker(publish, result, initial.failures or (initial.diagnostic,), dry_run)
                return 2
            good = known.stdout.strip()
            ancestor = subprocess.run(['git', 'merge-base', '--is-ancestor', good, candidate],
                                      cwd=root, timeout=10)
            if not initial.failures:
                result = f'INCONCLUSIVE main {candidate}: gate infrastructure failed: {initial.diagnostic}'
                code = 2
            elif good == candidate or ancestor.returncode or not run(good):
                result = ('INCONCLUSIVE main '+candidate+': '+
                          verdict_reason('previously green baseline also fails or history changed',
                                         initial.failures))
                code = 2
            elif (repeated := run(candidate)) or repeated.failures != initial.failures:
                result = ('INCONCLUSIVE main '+candidate+': '+
                          verdict_reason('candidate failure did not repeat', initial.failures))
                code = 2
            else:
                commits = git(root, 'rev-list', '--first-parent', '--reverse',
                              '--max-count', str(MAX_COMMITS+1), good+'..'+candidate).splitlines()
                if not commits or len(commits) > MAX_COMMITS:
                    result = ('INCONCLUSIVE main '+candidate+': '+
                              verdict_reason('no bounded first-parent range from green baseline',
                                             initial.failures))
                    code = 2
                else:
                    while len(commits) > 1:
                        midpoint = (len(commits)-1)//2
                        if run(commits[midpoint]):
                            commits = commits[midpoint+1:]
                        else:
                            commits = commits[:midpoint+1]
                    culprit = commits[0]
                    parent = git(root, 'rev-parse', culprit+'^')
                    parent_result = run(parent)
                    culprit_result = run(culprit)
                    if not parent_result or culprit_result or culprit_result.failures != initial.failures:
                        result = ('INCONCLUSIVE main '+candidate+': '+
                                  verdict_reason('parent is red or the attributed failure did not repeat',
                                                 initial.failures))
                        code = 2
                        tested = None
                    else:
                        tested = tested_revert(root, state, candidate, culprit, bounded_check)
                    if tested is not None:
                        reverted, qualification = tested
                        subject = git(root, 'show', '-s', '--format=%s', culprit)
                        name, email = git(root, 'show', '-s', '--format=%an%x00%ae', culprit).split('\0')
                        plan = dict(candidate=candidate, good=good, culprit=culprit, parent=parent,
                                    reverted=reverted, subject=subject, author_name=name, author_email=email,
                                    failures=list(initial.failures), red_log=str(initial.log) if initial.log else None,
                                    parent_log=str(parent_result.log) if parent_result.log else None,
                                    culprit_log=str(culprit_result.log) if culprit_result.log else None,
                                    gate_log=str(qualification.log) if qualification.log else None)
                        print('DRY RUN plan: ' if dry_run else 'Revert plan: ', json.dumps(plan), flush=True)
                        if dry_run:
                            result = f'RED culprit {culprit}: {subject}\nOwner: {name} <{email}>; dry run, canonical main unchanged.'
                            code = 1
                        elif publish is None:
                            raise RuntimeError('No canonical landing publisher configured')
                        else:
                            landed = publish(root, state, plan)
                            if landed != reverted:
                                raise RuntimeError('Landing result differs from the tested revert')
                            restore = landed
                            git(root, 'update-ref', GOOD_REF, landed)
                            result = f'REVERTED main {landed}: {subject}\nFailure: '+', '.join(initial.failures)
                            if getattr(publish, 'warning', None):
                                result += '\n'+publish.warning
                            code = 1 if getattr(publish, 'warning', None) else 0
                    elif parent_result and not culprit_result and culprit_result.failures == initial.failures:
                        result = ('INCONCLUSIVE main '+candidate+': '+
                                  verdict_reason('removing the bisection candidate did not restore a green full gate',
                                                 initial.failures))
                        code = 2
        save_result(state, result)
        if code == 2:
            report_blocker(publish, result, initial.failures or (initial.diagnostic,), dry_run)
        print(result, flush=True)
        return code
    except NightlySkipped as error:
        result = f'skipped: {error}; main {candidate}'
        save_result(state, result)
        print(result, flush=True)
        return 0
    except BaseException as error:
        detail = getattr(error, "nightly_diagnostic", None) or nightly_failure.record(
            state, error, commit=candidate, requested_signal=_STOP_SIGNAL)
        result = f'INCONCLUSIVE main {candidate}: runner failed: {detail}'
        save_result(state, result)
        report_blocker(publish, result, (detail,), dry_run)
        raise
    finally:
        if (root/'.git/REVERT_HEAD').exists() or (root/'.git/sequencer').exists():
            git(root, 'revert', '--abort')
        git(root, 'checkout', '--quiet', '--detach', restore)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', type=Path, required=True)
    parser.add_argument('--dry-run', action='store_true', help='diagnose and print the tested plan without landing or sending mail')
    parser.add_argument('--agent-id', default=os.environ.get('STATEENGINE_AGENT_ID'), help='explicit StateEngine actor for revert notifications')
    args = parser.parse_args()
    root = args.repository.resolve()
    marker = root/'.git/secode-nightly-owner'
    if not marker.is_file() or marker.is_symlink() or git(root,'status','--porcelain'):
        raise RuntimeError('Nightly requires its dedicated, marked, clean clone')
    if heavy.inherited() is not None:
        raise RuntimeError('Run the nightly supervisor outside a heavy lease; each full gate queues separately')
    publisher = None if args.dry_run else Publisher(args.agent_id)
    state = STATE
    (state/'last-failure.json').unlink(missing_ok=True)
    state.mkdir(parents=True, exist_ok=True)
    (state/'current-attempt.json').write_text(json.dumps(dict(commit=None, logs=[]))+'\n')
    save_result(state, 'RUNNING canonical main fetch')
    subprocess.run(['git','fetch','--quiet','--no-tags','origin','main'], cwd=root,
                   check=True, timeout=180)
    candidate = git(root,'rev-parse','FETCH_HEAD')
    deadline = time.monotonic()+AUDIT_TIMEOUT_SECONDS
    def check(root, state, commit):
        return full_gate(root, state, commit, deadline=deadline)
    return audit(root,state,candidate,check,dry_run=args.dry_run,publish=publisher,deadline=deadline)


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, request_stop)
    signal.signal(signal.SIGINT, request_stop)
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        detail = getattr(error, 'nightly_diagnostic', None) or nightly_failure.record(
            STATE, error, requested_signal=_STOP_SIGNAL)
        save_result(STATE, f'INCONCLUSIVE infrastructure failure: {detail}')
        print(f'Nightly infrastructure failure: {detail}', file=sys.stderr)
        raise SystemExit(2)
