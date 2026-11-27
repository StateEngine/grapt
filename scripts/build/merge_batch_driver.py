#!/usr/bin/env python3
"""Qualify accepted, disjoint slices together and land through the normal helper."""
from operator_paths import worktrees
import argparse
from contextlib import chdir, nullcontext
import fcntl
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import time

import gate
import gate_policy
import gate_retry
import heavy
import landing
import merge_batch
import full_capacity
from heavy_runs import metadata
from nightly_revert import quote, remote

_STOP_REQUESTED = False


def read_queue(path):
    with metadata(path, os.O_RDONLY, 'rb') as stream:
        raw = stream.read(65537)
    if len(raw) > 65536:
        raise ValueError('Accepted queue exceeds 64 KiB')
    return merge_batch.admissions(json.loads(raw))


def named_failure(log):
    raw = gate_retry.read_log(log).decode('utf-8', errors='replace')
    patterns = (r'^test (.{1,1024}) \.\.\. FAILED$',
                r'^# nextest failure: (.{1,2048})$',
                r'^(?:FAIL|ERROR): (.{1,1024})$',
                r'^error\[E\d{4}\]: (.{1,1024})$',
                r'^error: [^\n]{1,1024}\n(?:[^\n]{0,1024}\n){0,2}\s*--> [^\n]{1,1024}[.]rs:\d+:\d+')
    for pattern in patterns:
        match = re.search(pattern, raw, re.M)
        if match:
            return match[0]
    return None


class Runner:
    def __init__(self, options, state):
        self.options, self.state = options, state
        self.deadline = time.monotonic() + 24*3600
        self.index = 0

    def record(self, value):
        with (self.state/'results.jsonl').open('a', encoding='utf-8') as stream:
            stream.write(json.dumps(value)+'\n')
            stream.flush()
            os.fsync(stream.fileno())
        print(json.dumps(value), flush=True)

    def __call__(self, rows):
        self.index += 1
        if self.index > 2*merge_batch.MAX_CANDIDATES-1 or time.monotonic() >= self.deadline:
            return merge_batch.Outcome('blocked', 'Batch exceeded its attempt or 24-hour bound')
        options = self.options
        heavy.run(['git', 'fetch', '--no-tags', '--', options.canonical, 'main'],
                  cwd=options.repository, check=True, timeout=120)
        base = landing.git(options.repository, 'rev-parse', 'FETCH_HEAD').decode().strip()
        branch = f'secode-batch-{self.state.name}-{self.index}'
        checkout = options.worktrees/branch
        commit = merge_batch.compose(options.repository, checkout, branch, base, rows)
        # Preserve the exact combined branch until publication is resolved.
        # Failed/finished targets follow ordinary marked-target retention.
        mode = gate_policy.required_mode(landing.paths(checkout, base, commit),
                                         root=checkout, base=base, head=commit)
        log = self.state/f'{self.index}.gate.log'
        try:
            capacity = (full_capacity.reserve(min(10800, max(0, self.deadline-time.monotonic())), shared=True)
                        if mode == 'full' else nullcontext())
            with chdir(checkout), capacity, heavy.slot('landing',
                            wait=min(10800, max(.001, self.deadline-time.monotonic())),
                            job_owner=options.owner, ram=False,
                            nvme=heavy.nvme_target(Path(os.environ.get('SECODE_CACHE_DIR',
                                                 str(Path.home()/'.cache/secode')))/'target')) as lease:
                command, allowance = landing.gate_invocation(
                    checkout, gate, mode, base,
                    gate.FULL_GATE_TIMEOUT_SECONDS if mode == 'full' else gate.REVIEWED_GATE_TIMEOUT_SECONDS,
                    log)
                with (self.state/f'{self.index}.driver.log').open('x', encoding='utf-8') as driver:
                    gate.execute(command, driver, min(self.deadline, time.monotonic()+allowance),
                                 cwd=checkout, lease_fd=lease.fileno())
        except subprocess.CalledProcessError as error:
            failure = named_failure(log) if log.exists() and error.returncode == 1 else None
            return merge_batch.Outcome('red' if failure else 'blocked',
                                        f'{failure or "Qualification did not identify a source failure"}; {log}')
        landing.qualification_proof(checkout, log, commit, base, mode)
        for row in rows:
            landing.source(row.root, row.commit, row.branch)
        pending = self.state/'pending-publication.json'
        # A transport failure may leave the remote controller alive. Persist
        # this before launch and never start a second publication on uncertainty.
        with pending.open('x', encoding='utf-8') as stream:
            json.dump(dict(commit=commit, base=base, branch=branch, checkout=str(checkout)), stream)
            stream.flush(); os.fsync(stream.fileno())
        tier = '1' if any(row.tier == '1' for row in rows) else '2'
        script = (f'& {quote(options.controller)} -Branch {quote(branch)}'
                  f' -AcceptedCommit {quote(commit)} -LinuxRoot {quote(str(checkout))}'
                  f' -Owner {quote(options.owner)} -Tier {tier} -GateLog {quote(str(log))}'
                  f' -ExpectedBase {quote(base)};if(-not $? ){{exit 1}}')
        remaining = min(15000, self.deadline-time.monotonic())
        if remaining <= 0:
            return merge_batch.Outcome('blocked', 'Publication deadline expired; preserve pending publication')
        output = remote(script, timeout=remaining)
        with (self.state/f'{self.index}.landing.log').open('x', encoding='utf-8') as stream:
            stream.write(output)
        landed = re.findall(r'^LANDED ([0-9a-f]{40}) \(was ([0-9a-f]{40})\)\s*$', output, re.M)
        if landed != [(commit, base)]:
            return merge_batch.Outcome('blocked', 'Publication is unconfirmed; inspect pending-publication.json')
        heavy.run(['git', 'fetch', '--no-tags', '--', options.canonical, 'main'],
                  cwd=options.repository, check=True, timeout=120)
        current = landing.git(options.repository, 'rev-parse', 'FETCH_HEAD').decode().strip()
        landing.git(options.repository, 'merge-base', '--is-ancestor', commit, current)
        pending.unlink()
        return merge_batch.Outcome('landed', commit)


def run():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('queue', type=Path)
    parser.add_argument('--repository', required=True, type=Path,
                        help='Dedicated controller clone; owner checkouts are never rebased')
    parser.add_argument('--canonical', default=os.environ.get('SECODE_CANONICAL_REMOTE', 'https://github.com/StateEngine/grapt'))
    parser.add_argument('--controller', default=os.environ.get('SECODE_LANDING_CONTROLLER'),
                        required=not bool(os.environ.get('SECODE_LANDING_CONTROLLER')))
    parser.add_argument('--owner', required=True)
    parser.add_argument('--state', required=True, type=Path,
                        help='New attempt directory outside the repository; preserved on uncertainty')
    parser.add_argument('--worktrees', type=Path, default=worktrees())
    options = parser.parse_args()
    if (not re.fullmatch('agt-[a-z0-9-]{3,80}', options.owner) or
            not re.fullmatch('[a-z0-9-]{1,60}', options.state.name) or
            not options.state.is_absolute() or not options.repository.is_absolute() or
            options.state.is_relative_to(options.repository) or
            not options.worktrees.is_absolute() or not (options.repository/'.git').is_dir()):
        raise ValueError('Use a dedicated clone, explicit seat, and new external state directory')
    rows = read_queue(options.queue)
    if heavy.inherited_fds():
        raise ValueError('Start the batch controller outside a build lease; each qualification schedules itself')
    # One batch driver per dedicated clone. This is not another build scheduler.
    with (options.repository/'.git/secode-batch.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        options.state.mkdir(mode=0o700)
        runner = Runner(options, options.state)
        try:
            results = merge_batch.drain(rows, runner, runner.record)
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt) as error:
            runner.record(dict(status='cancelled' if _STOP_REQUESTED else 'blocked',
                               detail=f'{type(error).__name__}: {error}'))
            return 130 if _STOP_REQUESTED else 1
    return int(any(row['status'] != 'landed' and len(row['commits']) == 1 or
                   row['status'] == 'blocked' for row in results))


def main():
    global _STOP_REQUESTED
    _STOP_REQUESTED = False
    def stop(_signal, _frame):
        global _STOP_REQUESTED
        _STOP_REQUESTED = True
        # A second cancellation must not interrupt owned-process cleanup.
        for number in (signal.SIGTERM, signal.SIGINT):
            signal.signal(number,signal.SIG_IGN)
        raise KeyboardInterrupt
    previous = {number: signal.getsignal(number) for number in (signal.SIGTERM,signal.SIGINT)}
    try:
        for number in previous:
            signal.signal(number,stop)
        return run()
    except KeyboardInterrupt:
        return 130
    finally:
        for number, handler in previous.items():
            signal.signal(number,handler)


if __name__ == '__main__':
    raise SystemExit(main())
