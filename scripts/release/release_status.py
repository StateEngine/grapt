#!/usr/bin/env python3
"""Record and announce each release run's outcome, so a failure cannot hide.

A failed nightly release used to be visible only in the journal. After a reboot
`systemctl show` reports the unit's default state, which reads as success, and
nothing told the coordinator. This keeps two facts outside the unit's lifetime:

- a per-run status directory, written before and after every run, so the newest
  outcome is readable regardless of reboot, systemd state or journal rotation;
- a coordinator mail on failure, carrying the night, the commit, the first
  failing step and a short tail of its output.

The mail is best effort: a mail failure is recorded and reported, never turned
into a second failure or allowed to mask the first.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

STATUS_SCHEMA = 'grapt.release-run.v1'
# Kept small: this is a status record, not a log copy.
STATUS_BYTES = 64 * 1024
TAIL_LINES = 6
TAIL_BYTES = 1200
# Bounded like every other child this tooling runs.
MAIL_TIMEOUT = 20


def status_path(status_root, run_id):
    return Path(status_root) / f'{run_id}.json'


def tail_lines(path, limit=TAIL_BYTES):
    """The last few lines of a log without reading the whole file.

    A gate log can be hundreds of MiB and the recorder can run under an address
    space cap, so the file is seeked from its end rather than read and split.
    """
    try:
        with Path(path).open('rb') as stream:
            stream.seek(0, 2)
            size = stream.tell()
            stream.seek(max(0, size - limit))
            raw = stream.read(limit)
    except OSError as error:
        return [f'output unreadable: {type(error).__name__}: {error}']
    # The first line of a mid-file window is a fragment; drop it.
    lines = raw.decode('utf-8', errors='replace').splitlines()
    return lines[1:] if size > limit and lines else lines


def _bounded(text, limit=TAIL_BYTES):
    text = text or ''
    return text[-limit:] if len(text) > limit else text


def _tail(lines=TAIL_LINES):
    """The last few lines, bounded as a whole so a long line cannot bloat it."""
    return _bounded('\n'.join(_bounded(line, 300) for line in (lines or [])[-TAIL_LINES:]))


def write_status(status_root, run_id, row):
    """Publish one run's status whole, so a reader never sees a partial row."""
    root = Path(status_root)
    root.mkdir(parents=True, exist_ok=True)
    payload = dict(row)
    payload['schema'] = STATUS_SCHEMA
    payload['updated'] = int(time.time())
    raw = json.dumps(payload, sort_keys=True)
    if len(raw.encode()) > STATUS_BYTES:
        raise ValueError('Release status record exceeds its bound')
    staged = root / f'.{run_id}.next'
    staged.write_text(raw + '\n')
    staged.replace(status_path(root, run_id))
    return payload


def latest_status(status_root):
    """The most recently updated run row, or None when none was ever written."""
    root = Path(status_root)
    rows = []
    for path in sorted(root.glob('*.json')) if root.is_dir() else []:
        try:
            info = path.lstat()
            if not info.st_mode & 0o170000 == 0o100000 or info.st_size > STATUS_BYTES:
                continue
            value = json.loads(path.read_text())
            if value.get('schema') != STATUS_SCHEMA or not isinstance(value.get('updated'), int):
                continue
            rows.append(value)
        except (OSError, ValueError, UnicodeError):
            continue
    # `updated` is whole seconds, so two runs can tie. A real run's id is a UTC
    # stamp; a placeholder id is not, and a real run always supersedes one.
    def precedence(row):
        stamp = re.fullmatch(r'\d{8}T\d{6}Z', str(row.get('run', '')))
        return (row['updated'], 1 if stamp else 0, str(row.get('run', '')))
    rows.sort(key=precedence)
    return rows[-1] if rows else None


def failed(step, output, exit_code):
    """One failing step, named as the mail will name it."""
    return {'step': _bounded(step, 300), 'exit': exit_code, 'tail': _tail(output)}


def announcement(run_id, commit, failure, status_root):
    """The mail body for one failed run, built only from recorded facts."""
    lines = [
        f'Release run {run_id} FAILED for commit {commit}.',
        f'Failing step: {failure["step"]}',
        f'Exit: {failure["exit"]}',
    ]
    if failure.get('tail'):
        lines += ['', 'Tail:', '```', failure['tail'], '```']
    lines += ['',
              f'Status record: {status_path(status_root, run_id)}',
              'The staging directory was left in place; nothing was published.']
    return '\n'.join(lines)


def mail(actor, body, *, send=None):
    """Tell the coordinator. Returns None on success, else a short reason.

    The actor is supplied by the caller, as the disk alert requires: a release
    run has no registered identity of its own, so it must not borrow one from
    the registry.
    """
    try:
        if send is None:
            from nightly_revert import registry, send as sender
            _agents, coordinator = registry()
            sender(actor, coordinator, body, timeout=MAIL_TIMEOUT)
            return None
        send(body)
        return None
    except Exception as error:  # A mail failure must never mask the release failure.
        return _bounded(f'{type(error).__name__}: {error}', 300)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--status-root', required=True, type=Path)
    parser.add_argument('--run-id', required=True)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--step', required=True)
    parser.add_argument('--exit', type=int, required=True)
    parser.add_argument('--output', type=Path, help='captured output of the failing step')
    parser.add_argument('--actor', help='registered agent identity that sends the mail')
    parser.add_argument('--no-mail', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not re.fullmatch(r'[0-9A-Za-z._-]{1,64}', args.run_id):
        parser.error('run id must be a bounded plain name')
    if not re.fullmatch(r'[0-9a-f]{7,40}', args.commit):
        parser.error('commit must be a hex revision')
    if not args.no_mail and not re.fullmatch(r'agt-[a-z0-9-]{3,80}', args.actor or ''):
        parser.error('--actor must name the registered agent that sends the mail')
    output = []
    if args.output is not None:
        try:
            output = args.output.read_text(errors='replace').splitlines()
        except OSError as error:
            output = [f'output unreadable: {error}']
    failure = failed(args.step, output, args.exit)
    row = {'run': args.run_id, 'commit': args.commit, 'outcome': 'failed',
           'failure': failure, 'mail_outcome': None}
    if not args.no_mail:
        row['mail_outcome'] = mail(args.actor, announcement(
            args.run_id, args.commit, failure, args.status_root))
    write_status(args.status_root, args.run_id, row)
    print(json.dumps(row, sort_keys=True), file=sys.stderr)
    # The release job's own non-zero exit stays the run's exit; this helper never
    # converts a failure into success.
    return 1


if __name__ == '__main__':
    raise SystemExit(main())
