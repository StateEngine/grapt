#!/usr/bin/env python3
"""Retire only an owner's proved waiting launcher, preserving single-use retry credit."""
import argparse
import os
import signal
import time

from heavy import HEAVY_LOCK, QUEUE_PATH
from heavy_drain import Drain
from heavy_queue import Queue, identity


def retire(queue, ticket_id, owner, pid, start):
    # A pidfd binds the signal to this process even if its numeric PID is reused.
    descriptor = os.pidfd_open(pid)
    try:
        with queue.locked(time.monotonic() + 30):
            tickets, runs = queue.state()
            ticket = next((row for row in tickets if row['id'] == ticket_id), None)
            if (ticket is None or ticket['owner'] != owner or ticket['pid'] != pid
                    or ticket['start'] != start or identity(pid) != start):
                raise ValueError('Waiting launcher identity or owner changed; nothing was signalled')
            # Conversion waiters have already run work; they must finish normally.
            if ticket['holder'] or ticket_id in runs.active:
                raise ValueError('Launcher was already admitted; nothing was signalled')
            Drain(queue.directory, tickets).refund(ticket, event='waiter-replacement-refund')
            signal.pidfd_send_signal(descriptor, signal.SIGINT)
    finally:
        os.close(descriptor)
    # Let the old launcher's normal cleanup remove its ticket before a retry.
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if identity(pid) != start:
            with queue.locked(deadline):
                tickets, _ = queue.state()
                if any(row['id'] == ticket_id for row in tickets):
                    raise RuntimeError('Old ticket is still present; do not start a replacement')
            return ticket_id
        time.sleep(0.05)
    raise TimeoutError('Old launcher has not exited; do not start a replacement')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ticket', required=True)
    parser.add_argument('--owner', required=True, help='exact owner from your original job')
    parser.add_argument('--pid', type=int, required=True)
    parser.add_argument('--start', required=True, help='exact boot/start identity from the queue')
    args = parser.parse_args()
    token = retire(Queue(QUEUE_PATH, HEAVY_LOCK), args.ticket, args.owner, args.pid, args.start)
    print(f'Retired waiting launcher; single-use retry: --owner {args.owner} --refund {token}')
    print('Confirm the old background job exited, then rerun its unchanged command and profile once.')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, TimeoutError) as error:
        raise SystemExit(f'Waiter replacement refused: {error}') from error
