#!/usr/bin/env python3
"""Deliver the cleanup-failure alert, apart from the cleanup it describes.

A cleanup pass must return its own result promptly, and reaching the coordinator
means an SSH round trip that can stall. So the pass only records what happened,
and this separate short unit reads that record and speaks. Nothing here can
change a cleanup result, delay one, or raise into it.

Every failure is an `alert-unsent` row and a line on stderr, including a registry
reply of the wrong shape: an unavailable coordinator is a fact worth recording,
not an exception worth losing.
"""
import argparse
import contextlib
import fcntl
import math
import os
from pathlib import Path
import re
import stat
import sys
import time

import disk_stall
import hygiene_outcomes

RUN_SECONDS = 120
# This unit's own state: the streak it announced, so one outage is said once,
# and how many sends in a row failed, so a dead transport is visible on the box.
STATE = '.hygiene-alert.json'
LOCK = '.hygiene-alert.lock'
EMPTY = {'version': 1, 'streak_id': None, 'announced': None, 'alerted': None,
         'failed_sends': 0, 'unsent': None, 'unsent_kind': None}


def _lock(cache):
    """One deliverer at a time, without ever blocking on an odd lock file."""
    try:
        descriptor = os.open(cache / LOCK,
                             os.O_RDWR | os.O_CREAT | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC,
                             0o600)
    except OSError:
        return None
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            os.close(descriptor)
            return None
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        os.close(descriptor)
        return None
    return os.fdopen(descriptor, 'r+')


def _state(path):
    try:
        row = disk_stall.read(path, dict(EMPTY))
    except (OSError, ValueError):
        return dict(EMPTY)
    if (not isinstance(row, dict) or set(row) != set(EMPTY) or row['version'] != 1
            or type(row['failed_sends']) is not int or row['failed_sends'] < 0
            or any(row[key] is not None and not isinstance(row[key], str)
                   for key in ('streak_id', 'announced', 'unsent_kind'))
            or (row['alerted'] is not None and (type(row['alerted']) not in (int, float)
                or not math.isfinite(row['alerted']) or row['alerted'] < 0))):
        return dict(EMPTY)
    return row


def _registry(discover=None):
    """The coordinator, parsed defensively: any shape problem is a failure."""
    if discover is None:
        from nightly_revert import registry as discover
    reply = discover()
    if not isinstance(reply, tuple) or len(reply) != 2:
        raise ValueError('Registry reply has an unexpected shape')
    agents, coordinator = reply
    if (not isinstance(agents, list) or not isinstance(coordinator, str)
            or not re.fullmatch(r'agt-[a-z0-9-]{3,80}', coordinator)):
        raise ValueError('Registry reply has an unexpected shape')
    if not any(isinstance(row, dict) and row.get('agent_id') == coordinator
               for row in agents):
        raise ValueError('Registry reply names no coordinator')
    return coordinator


def _latest():
    """The newest pass row, or None. Delivery never infers a streak itself."""
    recorded = hygiene_outcomes.samples()
    return recorded[-1] if recorded else None


def _record_unsent(cache, state, error, now):
    """One unsent delivery: the deliverer's own state, plus a loud line."""
    state['failed_sends'] += 1
    state['unsent'] = {'observed_unix': now, 'detail': str(error)[:hygiene_outcomes.DETAIL_CHARS]}
    disk_stall.write(cache / STATE, dict(state))
    message = (f'Cleanup alert could not be sent ({state["failed_sends"]} in a row): '
               f'{error}. The cleaner is still refusing; inspect it locally.')
    print(message, file=sys.stderr, flush=True)
    if state['failed_sends'] >= hygiene_outcomes.ALERT_STREAK:
        print(f'WARNING: alert transport has failed {state["failed_sends"]} times; '
              'no coordinator has been told about the cleanup outage.',
              file=sys.stderr, flush=True)
    return message


def deliver(cache, actor, now=None, send=None, registry=None):
    """Speak once for a continuing outage; report, never raise, on any failure."""
    now = time.time() if now is None else now
    cache = Path(cache)
    cache.mkdir(parents=True, exist_ok=True)
    latest = _latest()
    if latest is None or latest['streak_len'] < hygiene_outcomes.ALERT_STREAK:
        # A recovery is a fact of its own pass row, so this clears the announced
        # identity whether or not a tick ran during the recovered interval.
        with contextlib.suppress(OSError):
            (cache / STATE).unlink()
        return None
    held = _lock(cache)
    if held is None:
        return None
    with held:
        path = cache / STATE
        state = _state(path)
        streak_id = hygiene_outcomes.streak_id(latest)
        if state.get('announced') == streak_id:
            return None
        body = (f'SE Code cleanup has refused {latest["streak_len"]} passes in a row (threshold '
                f'{hygiene_outcomes.ALERT_STREAK}). Last reason: {latest["detail"] or "no detail recorded"}. '
                'The hourly cleaner is not reclaiming disk. Inspect journalctl --user '
                '-u secode-disk-hygiene.service and the cache census before the floor.')
        try:
            if send is None:
                from nightly_revert import send as send
            send(actor, _registry(registry), body, timeout=20)
        # Delivery is telemetry: any failure at all is one unsent record, never a
        # crash and never a lost cleanup result. Discovery and send timeouts arrive
        # as subprocess.TimeoutExpired, so the catch is deliberately total.
        except Exception as error:  # noqa: BLE001 - telemetry must never fail work
            state['unsent_kind'] = type(error).__name__
            _record_unsent(cache, state, error, now)
            return None
        state.update(streak_id=streak_id, announced=streak_id, alerted=now,
                     failed_sends=0, unsent=None, unsent_kind=None)
        disk_stall.write(path, dict(state))
        print(body, file=sys.stderr, flush=True)
        return body


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--agent-id', required=True,
                        help='explicit StateEngine identity that sends the alert')
    parser.add_argument('--cache', type=Path,
                        help='cache whose pass record is read; defaults to the configured one')
    args = parser.parse_args()
    cache = (args.cache or Path(os.environ.get('SECODE_CACHE_DIR',
                                               Path.home() / '.cache/secode'))).resolve()
    started = time.monotonic()
    try:
        deliver(cache, args.agent_id)
    except (OSError, ValueError, RuntimeError) as error:
        print(f'Cleanup alert delivery failed: {error}', file=sys.stderr)
        return 1
    finally:
        if time.monotonic() - started > RUN_SECONDS:
            print('Cleanup alert delivery exceeded its bound', file=sys.stderr)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
