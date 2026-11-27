"""Persist and rate-limit a loud disk-floor stall warning beside the queue."""
import math
import fcntl
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import time

from heavy_hardware import ceiling, read, write

ALERT_SECONDS = 300
REPEAT_SECONDS = 1800
CAPACITY_WARNING_BYTES = 100_000_000_000
CAPACITY_ALERT_BYTES = 80 * 1024**3
CAPACITY_CHANGE_BYTES = 5 * 1024**3
CAPACITY_CHANGE_CEILING_BYTES = 100 * 1024**3
DISK_PRESSURE_LIMIT = 6



def current(directory, now=None, boot=None):
    now = time.monotonic() if now is None else now
    boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip() if boot is None else boot
    value = read(Path(directory) / 'disk-stall.json', {'version': 1, 'stall': None})
    if not isinstance(value, dict) or set(value) != {'version', 'stall'} or value['version'] != 1:
        raise ValueError('Invalid disk stall state')
    row = value['stall']
    if row is None:
        return None
    if (not isinstance(row, dict) or set(row) != {'boot', 'id', 'owner', 'since', 'updated', 'alerted', 'root_free', 'nvme_free'}
            or not isinstance(row['boot'], str) or len(row['boot']) > 64
            or not isinstance(row['id'], str) or not re.fullmatch('[0-9-]{1,80}', row['id'])
            or not isinstance(row['owner'], str) or not re.fullmatch('[a-zA-Z0-9_.:@-]{1,128}', row['owner'])
            or any(type(row[key]) not in (int, float) or not math.isfinite(row[key]) or row[key] < 0
                   for key in ('since', 'updated', 'root_free', 'nvme_free'))
            or row['alerted'] is not None and (type(row['alerted']) not in (int, float)
                or not math.isfinite(row['alerted']) or row['alerted'] < 0)):
        raise ValueError('Invalid disk stall observation')
    return row if row['boot'] == boot and row['since'] <= row['updated'] <= now else None


def message(row, now):
    return (f"QUEUE STALLED ON DISK FLOOR for {int(now-row['since'])}s: "
            f"{row['owner']}; root free={int(row['root_free'])}, "
            f"NVMe free={int(row['nvme_free'])}. Maintenance runs independently; "
            'the lead needs to reclaim protected/unmanaged space if it cannot recover the floor.')


def observe(directory, ticket, host, reason, now=None, boot=None):
    """Called under Queue.locked; records only, with no network or subprocess."""
    now = time.monotonic() if now is None else now
    boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip() if boot is None else boot
    row = current(directory, now, boot)
    if reason != 'disk free-space floor':
        if row is not None:
            write(Path(directory) / 'disk-stall.json', {'version': 1, 'stall': None})
        return None
    if row is None or row['id'] != ticket['id']:
        row = dict(boot=boot, id=ticket['id'], owner=ticket['owner'],
                   since=now, updated=now, alerted=None, root_free=host['root_free'],
                   nvme_free=host['nvme_free'])
    row.update(updated=now, root_free=host['root_free'], nvme_free=host['nvme_free'])
    alert = (now-row['since'] >= ALERT_SECONDS
             and (row['alerted'] is None or now-row['alerted'] >= REPEAT_SECONDS))
    if alert:
        row['alerted'] = now
    write(Path(directory) / 'disk-stall.json', {'version': 1, 'stall': row})
    if alert:
        warning = message(row, now)
        print(warning, file=sys.stderr, flush=True)
        return warning
    return None


def reduce_admission():
    """Lower future admissions only; never interrupt an existing build."""
    import heavy
    queue = heavy.Queue(heavy.QUEUE_PATH, heavy.HEAVY_LOCK)
    with queue.locked(time.monotonic()+5):
        changed = False
        for legacy, name in ((False, 'hardware-ceiling-v2.json'), (True, 'hardware-ceiling.json')):
            if ceiling(queue.directory, legacy=legacy) > DISK_PRESSURE_LIMIT:
                write(queue.directory/name, {'version': 1, 'limit': DISK_PRESSURE_LIMIT,
                      'reason': f'Retained {DISK_PRESSURE_LIMIT}-job ceiling from a prior disk-pressure warning; '
                                'current free space is sampled separately'})
                changed = True
        if changed:
            queue.notify(queue.load())


def capacity_warning(cache, actor, now=None, notify=None):
    """Warn independently of long cleanup runs, before build admission hits its floor."""
    if not isinstance(actor, str) or not re.fullmatch(r'agt-[a-z0-9-]{3,80}', actor):
        raise ValueError('Disk alerts require an explicit StateEngine agent')
    now = time.time() if now is None else now
    cache = Path(cache).resolve()
    cache.mkdir(parents=True, exist_ok=True)
    with (cache/'.disk-pressure.lock').open('a') as held:
        try:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return None
        free = shutil.disk_usage(cache).free
        root_free = shutil.disk_usage(Path('/')).free
        if free < CAPACITY_WARNING_BYTES:
            reduce_admission()
        path = cache/'.disk-pressure.json'
        row = read(path, {'version': 1, 'attempted': None, 'sent': None})
        if (not isinstance(row, dict) or set(row) != {'version', 'attempted', 'sent'}
                or row['version'] != 1 or any(value is not None and
                    (type(value) not in (int, float) or not math.isfinite(value) or value < 0)
                    for value in (row['attempted'], row['sent']))):
            raise ValueError('Invalid disk pressure alert state')
        if any(value is not None and value > now for value in (row['attempted'], row['sent'])):
            row = {'version': 1, 'attempted': None, 'sent': None}
        sample_path = cache/'.disk-pressure-free.json'
        sample = read(sample_path, None)
        if sample is not None and (
                not isinstance(sample, dict) or set(sample) != {'version', 'root_free', 'nvme_free'}
                or sample['version'] != 1 or any(type(sample[key]) not in (int, float)
                    or not math.isfinite(sample[key]) or sample[key] < 0
                    for key in ('root_free', 'nvme_free'))):
            raise ValueError('Invalid disk pressure free-space baseline')
        observed = {'version': 1, 'root_free': root_free, 'nvme_free': free}
        if sample is None:
            # Old alert timestamps lack size samples. Establish a quiet baseline
            # rather than announce unchanged headroom during migration.
            sample = observed
            write(sample_path, sample)
        changed = any(observed[key] < CAPACITY_CHANGE_CEILING_BYTES
                      and abs(observed[key]-sample[key]) > CAPACITY_CHANGE_BYTES
                      for key in ('root_free', 'nvme_free'))
        if min(free, root_free) >= CAPACITY_ALERT_BYTES and not changed:
            return None
        if ((row['attempted'] is not None and now-row['attempted'] < 300)
                or (row['sent'] is not None and now-row['sent'] < REPEAT_SECONDS)):
            return None
        body = (f'SE Code disk pressure: root has {root_free/1024**3:.1f} GiB free; '
                f'cache/NVMe has {free/1024**3:.1f} GiB free. '
                'An alert requires less than 80 GiB free or a change greater than 5 GiB '
                'on a disk below 100 GiB; '
                'the 60 GB admission floor remains. '
                'Inspect cache reclamation and protected space before the admission floor is reached.')
        row['attempted'] = now
        write(path, row)
        if notify is None:
            from nightly_revert import registry, send
            _agents, coordinator = registry()
            notify = lambda message: send(actor, coordinator, message, timeout=20)
        notify(body)
        row['sent'] = now
        write(path, row)
        write(sample_path, observed)
        print(body, file=sys.stderr, flush=True)
        return body
