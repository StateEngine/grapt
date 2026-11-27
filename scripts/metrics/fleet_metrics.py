#!/usr/bin/env python3
"""Export bounded operational observations; never inspect seat conversations."""
import argparse
import base64
from datetime import datetime
import json
import math
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]/'build'))
import operator_paths

LIMIT = 1024 * 1024


def read(path, maximum=LIMIT):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError('source is not a regular file')
        raw = stream.read(maximum + 1)
    if len(raw) > maximum:
        raise ValueError('source exceeds byte limit')
    return raw


def command(argv, timeout=15):
    """Bound duration and output while draining a child without a shell."""
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                               start_new_session=True)
    raw = bytearray()
    completed = False
    try:
        deadline = time.monotonic() + timeout
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError('probe timed out')
                for key, _ in selector.select(remaining):
                    chunk = os.read(key.fd, min(16384, LIMIT + 1 - len(raw)))
                    if not chunk:
                        selector.unregister(key.fileobj)
                    raw.extend(chunk)
                    if len(raw) > LIMIT:
                        raise ValueError('probe output exceeds byte limit')
        if process.wait(timeout=max(0.001, deadline - time.monotonic())):
            raise ValueError('probe returned an unsuccessful status')
        completed = True
        return raw.decode('utf-8-sig').strip()
    finally:
        if not completed:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
        process.stdout.close()


def observe(probe):
    try:
        return {'state': 'ok', 'value': probe()}
    except (OSError, ValueError, KeyError, TypeError, IndexError, ImportError, AttributeError,
            TimeoutError, subprocess.SubprocessError) as error:
        # Do not copy command output, paths, or exception payloads into public JSON.
        return {'state': 'unavailable', 'reason': type(error).__name__, 'value': None}


def heavy(directory, launcher):
    # Reuse validated queue readers. snapshot()/state() reap and WRITE metadata;
    # load()/reap() plus Runs only read and build an in-memory projection.
    sys.path.insert(0, str(launcher))
    from heavy_queue import Queue
    from heavy_runs import Runs, metadata
    from heavy_hardware import Profiles
    if not (directory / 'tickets.json').exists() or not (directory / 'runs.json').exists():
        raise FileNotFoundError('queue has not been observed')
    queue = Queue(directory, Path.home() / 'secode-heavy.lock')
    with metadata(directory / 'mutex', os.O_RDONLY, 'rb') as lock:
        subprocess.run(['flock', '--shared', '--wait', '2', str(lock.fileno())],
                       pass_fds=(lock.fileno(),), check=True, timeout=3,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        tickets = queue.reap(queue.load())
        runs = Runs(directory, tickets)
        profiles = Profiles(directory, tickets, runs)
    holders = {row['id'] for row in runs.holders(tickets)}
    now = time.monotonic_ns()
    waits = [{'owner': row['owner'], 'seconds': round(max(0, now - row['enqueued_ns']) / 1e9, 1)}
             for row in tickets if row['id'] not in holders]
    logs = [row['log'] for row in profiles.value['clients'].values() if row['log']]
    return {'depth': len(waits), 'running': len(holders), 'waits': waits, 'gate_logs': logs}


def gates(directory, now, watched=None):
    paths, truncated = [], False
    with os.scandir(directory) as entries:
        for index, entry in enumerate(entries):
            if index == 32768:
                truncated = True
                break
            if entry.name.endswith('.receipt.json') and entry.is_file(follow_symlinks=False):
                stamp = entry.stat(follow_symlinks=False).st_mtime
                if stamp >= now - 3600:
                    paths.append((stamp, Path(entry.path)))
    known = {str(path) for _, path in paths}
    for log in watched or {}:
        path = Path(log + '.receipt.json')
        if str(path) not in known:
            try:
                stamp = path.lstat().st_mtime
                if stamp >= now - 3600:
                    paths.append((stamp, path))
            except FileNotFoundError:
                pass  # The observed gate has not published a successful result.
    paths.sort(reverse=True)
    truncated |= len(paths) > 128
    records, unreadable = [], 0
    for _, path in paths[:128]:
        try:
            row = json.loads(read(path, 65536))
            # Box receipts keep their schema until the operator transition.
            if row.get('schema') != 'secode.gate.v1':  # grapt-rename: keep
                continue
            if (type(row.get('exit')) is not int or row['exit'] != 0
                    or row.get('status', 'PASS') not in ('PASS', 'PASS_WITH_RETRY')):
                continue
            finished, seconds = row['finished_unix'], row['seconds']
            if not all(type(x) in (int, float) and math.isfinite(x) for x in (finished, seconds)):
                raise ValueError('invalid gate duration')
            if not now - 3600 <= finished <= now or seconds < 0:
                continue
            retries = row.get('retries', [])
            if not isinstance(retries, list):
                raise ValueError('invalid retry list')
            records.append({'finished_unix': finished, 'mode': row['mode'],
                            'seconds': seconds, 'flaky_retries': len(retries)})
        except (OSError, ValueError, KeyError, TypeError):
            unreadable += 1
    return {'window_seconds': 3600, 'coverage': 'partial' if truncated or unreadable else 'observed-sources',
            'source': str(directory), 'watching': watched or {}, 'unreadable': unreadable, 'recent': records,
            'flaky_retries': sum(row['flaky_retries'] for row in records)}


def host():
    memory = {}
    for line in read('/proc/meminfo', 65536).decode().splitlines():
        key, value = line.split(':', 1)
        memory[key] = int(value.split()[0]) * 1024
    disks = {}
    for name, path in [('root', '/'), ('nvme', os.environ['SECODE_NVME_ROOT']), ('tmp', '/tmp')]:
        disk = shutil.disk_usage(path)
        disks[name] = {'free_bytes': disk.free, 'total_bytes': disk.total}
    return {'load_1_5_15': list(os.getloadavg()), 'ram_total_bytes': memory['MemTotal'],
            'ram_available_bytes': memory['MemAvailable'], 'disks': disks}


def windows(alias):
    if not re.fullmatch(r'[a-zA-Z0-9_-]{1,80}', alias):
        raise ValueError('invalid SSH alias')
    code = r"""$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'
$result = @{}
try { $result.disks = @(Get-PSDrive -Name C,E | Select-Object Name,Free,Used) } catch { $result.disks = $null }
try {
    & git -C $env:SECODE_CANONICAL_REPOSITORY reflog exists refs/heads/main
    if ($LASTEXITCODE -ne 0) { throw 'no reflog' }
    $rows = @(& git -C $env:SECODE_CANONICAL_REPOSITORY reflog show '--since=1 hour ago' --date=unix '--format=%gD' -n 513 refs/heads/main)
    if ($LASTEXITCODE -ne 0 -or $rows.Count -gt 512) { throw 'reflog unavailable or truncated' }
    $result.landings_per_hour = $rows.Count
} catch { $result.landings_per_hour = $null }
try {
    $status = (& se status --project secode --json | Out-String | ConvertFrom-Json)
    if ($LASTEXITCODE -ne 0) { throw 'status unavailable' }
    $result.seats = $status.sections.seats
} catch { $result.seats = $null }
$result | ConvertTo-Json -Depth 12 -Compress
"""
    encoded = base64.b64encode(operator_paths.powershell_environment(code).encode('utf-16-le')).decode()
    return json.loads(command(['ssh', '-oBatchMode=yes', '-oConnectTimeout=5', alias,
                               'powershell', '-NoProfile', '-EncodedCommand', encoded], timeout=35))


def seats(source, now, previous):
    if not isinstance(source, dict) or source.get('coverage') != 'observed':
        raise ValueError('seat source unavailable')
    observed = datetime.fromisoformat(source['observed_at'].replace('Z', '+00:00')).timestamp()
    if not 0 <= now - observed <= 600:
        raise ValueError('seat observation stale')
    rows = source['per_seat']
    if not isinstance(rows, dict) or len(rows) > 128:
        raise ValueError('seat inventory exceeds bound')
    result = []
    prior = {}
    if previous and 0 <= now - previous.get('observed_unix', 0) <= 600:
        prior = {row['agent_id']: row for row in
                 ((previous.get('seats') or {}).get('value') or {}).get('rows', [])}
    for agent, row in rows.items():
        state = row.get('state', 'unknown')
        # The source omits completion timestamps. Report the observed span of
        # consecutive idle samples, explicitly distinct from exact idle time.
        last = prior.get(agent, {})
        since = last.get('idle_observed_since_unix') if last.get('state') == 'idle' else None
        if state == 'idle':
            since = since if isinstance(since, (int, float)) and 0 <= since <= now else observed
        else:
            since = None
        result.append({'agent_id': agent, 'state': state,
                       'idle_seconds': 0 if state == 'working' else None,
                       'idle_time_state': 'sampled' if state == 'idle' else
                           'working' if state == 'working' else 'not-reported',
                       'idle_observed_since_unix': since,
                       'sampled_idle_seconds': round(max(0, observed - since), 1) if since else None})
    return {'observed_at': source['observed_at'], 'rows': result}


def cache():
    data = json.loads(command(['sccache', '--show-stats', '--stats-format', 'json']))['stats']
    hits = sum(data['cache_hits']['counts'].values())
    misses = sum(data['cache_misses']['counts'].values())
    return {'hits': hits, 'misses': misses, 'read_share': hits / (hits + misses) if hits + misses else None,
            'compile_requests': data['compile_requests']}


def collect(args, clock=time.time):
    remote = observe(lambda: windows(args.windows_host))
    value = remote['value'] or {}
    now = clock()
    try:
        previous = json.loads(read(args.output, 256 * 1024))
        if not isinstance(previous, dict) or previous.get('schema') != 'grapt.fleet.v1':
            previous = None
    except (OSError, ValueError):
        previous = None
    source = value.pop('seats', None)
    queue = observe(lambda: heavy(args.queue, args.launcher))
    watched = {}
    if previous:
        candidates = ((previous.get('gates') or {}).get('value') or {}).get('watching', {})
        if isinstance(candidates, dict):
            watched = {path: seen for path, seen in candidates.items()
                       if isinstance(path, str) and len(path) <= 4096 and Path(path).is_absolute()
                       and type(seen) in (int, float) and now - 3600 <= seen <= now}
    for path in (queue['value'] or {}).get('gate_logs', []):
        watched[path] = now
    watched = dict(sorted(watched.items(), key=lambda item: item[1], reverse=True)[:128])
    return {'schema': 'grapt.fleet.v1', 'observed_unix': now, 'cadence_seconds': 300,
            'heavy': queue,
            'gates': observe(lambda: gates(args.gate_logs, now, watched)),
            'host': observe(host),
            'windows': remote,
            'seats': observe(lambda: seats(source, now, previous)),
            'sccache': observe(cache),
            'nextest': observe(lambda: command(['cargo-nextest', '--version']).splitlines()[0])}


def publish(destination, value):
    encoded = json.dumps(value, allow_nan=False, separators=(',', ':')) + '\n'
    if len(encoded.encode()) > 256 * 1024:
        raise ValueError('fleet snapshot exceeds 256 KiB')
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=destination.parent,
                                         prefix='.fleet-', delete=False) as output:
            temporary = Path(output.name)
            output.write(encoded)
            output.flush()
            os.fsync(output.fileno())
        temporary.chmod(0o644)
        temporary.replace(destination)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def data_root():
    """Rename default: prefer the grapt data root, fall back to the legacy
    secode root, else the renamed path."""
    override = os.environ.get('SECODE_DATA_ROOT')
    if override is not None:
        return Path(override)
    base = Path.home()/'.local'/'share'
    if (base/'grapt').is_dir():
        return base/'grapt'
    if (base/'secode').is_dir():
        return base/'secode'
    return base/'grapt'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, default=data_root()/'metrics/fleet.json')
    parser.add_argument('--queue', type=Path, default=Path.home() / '.local/state/secode/heavy')
    parser.add_argument('--launcher', type=Path, default=Path.home() / '.local/lib/secode-release')
    parser.add_argument('--gate-logs', type=Path, default=Path('/tmp'))
    parser.add_argument('--windows-host', default='windows')
    args = parser.parse_args()
    publish(args.output, collect(args))


if __name__ == '__main__':
    main()
