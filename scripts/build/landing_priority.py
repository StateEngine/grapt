"""Protect landing guests through the existing queue and durable job owner."""
import fcntl
import os
from pathlib import Path
import re
import subprocess
import time

import durable_jobs
import landing_guests
import landing_reservation
from heavy_hardware import read, write, ceiling, SANITY_LIMIT
from heavy_queue import Queue, identity
from heavy_runs import metadata

CEILINGS = ('hardware-ceiling.json', 'hardware-ceiling-v2.json')
CLAMP = dict(version=1, limit=1, reason='landing guest RAM reserve')


def valid_ceiling(row):
    return (row is None or isinstance(row, dict) and set(row) == {'version', 'limit', 'reason'}
            and row['version'] == 1 and type(row['limit']) is int and 1 <= row['limit'] <= SANITY_LIMIT
            and isinstance(row['reason'], str) and len(row['reason']) <= 1024)


def available():
    raw = landing_guests.bounded(Path('/proc/meminfo'), 16384).decode()
    rows = dict(line.split(':', 1) for line in raw.splitlines())
    value = int(rows['MemAvailable'].split()[0])*1024
    if value <= 0:
        raise ValueError('Host available memory is unavailable')
    return value


class Fence:
    """Restore ordinary policy; unknown state alone needs a legacy-client fence."""
    def __init__(self, queue, handle):
        self.queue, self.handle = queue, handle
        self.path = queue.directory/'landing-priority-ceilings.json'

    def update(self, held, *, unknown=False):
        try:
            self.restore_or_hold(held, unknown=unknown)
        except (OSError, ValueError):
            # A damaged restore record cannot authorize ordinary admission.
            # Preserve it for repair while both generations retain ceiling=1.
            # Do not invent a replacement restore policy from damaged data.
            self.engage(unknown=True)
            raise

    def engage(self, *, unknown=False):
        if unknown:
            try:
                fcntl.flock(self.handle, fcntl.LOCK_SH | fcntl.LOCK_NB)
            except BlockingIOError:
                pass  # A running exclusive already fences every new admission.
        else:
            # A RAM reserve is not an ordinary job. Its own lease must not
            # consume the sole reduced slot while real work fits the RAM gate.
            fcntl.flock(self.handle, fcntl.LOCK_UN)
        errors = []
        for name in CEILINGS:
            try:
                write(self.queue.directory/name, CLAMP)
            except (OSError, ValueError) as error:
                errors.append(error)
        if errors:
            raise errors[0]

    def restore_or_hold(self, held, *, unknown=False):
        # Queue.locked must own this operation. The backup survives a service
        # restart; operator/disk-pressure reductions are never raised on restore.
        saved = read(self.path, None)
        if saved is not None and (not isinstance(saved, dict) or set(saved) != set(CEILINGS)
                                  or not all(valid_ceiling(row) for row in saved.values())):
            raise ValueError('Invalid landing priority ceiling backup')
        if held:
            previous = dict(saved or {})
            for index, name in enumerate(CEILINGS):
                ceiling(self.queue.directory, legacy=index == 0)
                current = read(self.queue.directory/name, None)
                if current != CLAMP:
                    old = previous.get(name)
                    if old is None or current is not None and current['limit'] < old['limit']:
                        previous[name] = current
                    elif name not in previous:
                        previous[name] = current
                elif name not in previous:
                    raise ValueError('Landing priority clamp has no restore record')
            write(self.path, previous)
            self.engage(unknown=unknown)
        else:
            if saved is not None:
                for name, old in saved.items():
                    if read(self.queue.directory/name, None) == CLAMP:
                        if old is None:
                            (self.queue.directory/name).unlink()
                        else:
                            write(self.queue.directory/name, old)
                self.path.unlink()
            fcntl.flock(self.handle, fcntl.LOCK_UN)


def newest(tickets, runs):
    rows = [ticket for ticket in runs.holders(tickets)
            if ticket['class'] == 1 and ticket['owner'] != 'native-windows-vm'
            and runs.active.get(ticket['id'], {}).get('since') is not None]
    return max(rows, key=lambda row: (runs.active[row['id']]['since'], row['id']), default=None)


def service(ticket, proc=Path('/proc')):
    """Bind a registered wrapper to its exact existing durable service."""
    if ticket['class'] != 1 or ticket['owner'] == 'native-windows-vm':
        raise ValueError('Landing priority may cancel development holders only')
    if identity(ticket['pid']) != ticket['start']:
        return None
    process = proc/str(ticket['pid'])
    if process.stat().st_uid != os.getuid():
        raise ValueError('Development holder is not owned by this scheduler')
    raw = landing_guests.bounded(process/'environ', 1024*1024)
    values = [entry.partition(b'=')[2] for entry in raw.split(b'\0')
              if entry.startswith(b'SECODE_DURABLE_JOB=')]
    if len(values) != 1 or not re.fullmatch(b'[0-9a-f]{32}', values[0]):
        raise ValueError('Development holder has no exact durable job')
    token = values[0].decode()
    path = durable_jobs.directory(token)
    request = durable_jobs.request_for(path, token)
    if request.get('owner') != ticket['owner']:
        raise ValueError('Development holder and durable owner differ')
    groups = landing_guests.bounded(process/'cgroup', 8192).decode().splitlines()
    if not any(row.partition('::')[2].split('/')[-1] == request['unit'] for row in groups):
        raise ValueError('Development holder is outside its durable service')
    if (path/'result.json').exists() or identity(ticket['pid']) != ticket['start']:
        return None
    return token, request['unit']


def cancel(ticket, pending_path):
    bound = service(ticket)
    if bound is None:
        return None
    token, unit = bound
    pending = dict(ticket=ticket['id'], token=token, unit=unit)
    write(pending_path, pending)
    path = durable_jobs.directory(token)
    interruption = path/'capacity-interruption.json'
    if not interruption.exists():
        durable_jobs.write(interruption, dict(job=token, reason='landing guest RAM reserve',
                                             ticket=ticket['id'], interrupted_unix=time.time()))
    # Use the same control owner as durable_jobs --cancel. Nonblocking stop
    # leaves systemd's existing descendant join/cleanup deadline in charge.
    subprocess.run(['systemctl', '--user', '--no-block', 'stop', unit],
                   check=True, timeout=5, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    print(f"Landing reserve: {ticket['owner']} yields; durable job {token}", flush=True)
    return pending


def joined(pending):
    path = durable_jobs.directory(pending['token'])
    request = durable_jobs.request_for(path, pending['token'])
    try:
        result = durable_jobs.read(path/'result.json')
    except FileNotFoundError:
        return False
    if (result.get('job') != pending['token'] or result.get('owner') != request.get('owner')
            or result.get('source') != request.get('source')
            or type(result.get('exit')) is not int or not 0 <= result['exit'] <= 255):
        raise ValueError('Landing priority cleanup has no matching terminal record')
    return durable_jobs.service_state(pending['unit']) in ('inactive', 'failed')


class Monitor:
    def __init__(self, queue, handle):
        self.queue, self.fence = queue, Fence(queue, handle)
        self.pending_path = queue.directory/'landing-priority-pending.json'
        self.pending = read(self.pending_path, None)
        if self.pending is not None and (
                not isinstance(self.pending, dict) or set(self.pending) != {'ticket', 'token', 'unit'}
                or not isinstance(self.pending['ticket'], str)
                or not re.fullmatch('[0-9-]{1,80}', self.pending['ticket'])
                or not isinstance(self.pending['token'], str)
                or not re.fullmatch('[0-9a-f]{32}', self.pending['token'])
                or self.pending['unit'] != 'secode-job-'+self.pending['token']+'.service'):
            raise ValueError('Invalid landing priority pending cleanup')
        self.blocked = False
        self.retried = 0

    def step(self, guests, memory):
        # A failed census (None) fences admissions but never authorizes a stop.
        floor = landing_guests.MARGIN + sum(
            max(0, guest['memory_bytes']-guest['rss']) for guest in guests or [])
        threshold = floor
        if self.blocked:
            threshold += 4*1024**3  # Hysteresis avoids admission/stop oscillation.
        self.blocked = guests is None or bool(guests) and memory < threshold
        with self.queue.locked(time.monotonic()+5):
            tickets, runs = self.queue.state()
            self.fence.update(self.blocked, unknown=guests is None)
            candidate = newest(tickets, runs)
            live = {row['id'] for row in runs.holders(tickets)}
            self.queue.notify(tickets)
        if self.pending is not None:
            # A removed ticket alone is not joined cleanup. Wait for the durable
            # terminal record AND stopped service before choosing another job.
            state = durable_jobs.service_state(self.pending['unit'])
            if self.pending['ticket'] in live or not joined(self.pending):
                if (guests and memory < floor
                        and state == 'active' and time.monotonic()-self.retried >= 15):
                    selected = next((row for row in tickets if row['id'] == self.pending['ticket']), None)
                    if selected is not None and service(selected) == (self.pending['token'], self.pending['unit']):
                        self.retried = time.monotonic()
                        cancel(selected, self.pending_path)
                return
            self.pending = None
            self.pending_path.unlink()
        if guests and memory < floor and candidate is not None:
            try:
                self.pending = cancel(candidate, self.pending_path)
            finally:
                # Publication precedes stop, so a timeout or service restart
                # cannot move on to cancelling a second development job.
                self.pending = read(self.pending_path, None)


def main():
    import heavy
    queue = Queue(heavy.QUEUE_PATH, heavy.HEAVY_LOCK)
    with metadata(queue.directory/'landing-priority.lock', os.O_CREAT | os.O_RDWR, 'a') as singleton:
        fcntl.flock(singleton, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with metadata(queue.heavy, os.O_CREAT | os.O_RDWR, 'a') as lease:
            monitor = Monitor(queue, lease)
            reservation = landing_reservation.Reader(queue.directory/'landing-reservation-observed.json')
            last_error = None
            while True:
                try:
                    try:
                        guests = reservation.observe()
                    except (OSError, ValueError) as error:
                        monitor.step(None, 0)
                        raise RuntimeError('Landing guest census unavailable: '+str(error)) from error
                    try:
                        memory = available()
                    except (OSError, ValueError) as error:
                        monitor.step(None, 0)
                        raise RuntimeError('Landing priority memory sample unavailable: '+str(error)) from error
                    monitor.step(guests, memory)
                    last_error = None
                except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                    if str(error) != last_error:
                        print(str(error), flush=True)
                        last_error = str(error)
                time.sleep(1)


if __name__ == '__main__':
    main()
