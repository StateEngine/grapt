"""Observe host-wide gates and lease a fourth heavy slot with automatic rollback."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import signal
import statistics
import stat
import tempfile
import time


MAX_BYTES = 1024 * 1024
MAX_PROCESSES = 32768
MAX_ACTIVE = 64
HEARTBEAT_SECONDS = 30


@contextmanager
def regular_input(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(descriptor, 'rb') as stream:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            raise ValueError(f'Capacity evidence must be a regular file: {path}')
        yield stream, info


def read_json(path, limit=MAX_BYTES):
    with regular_input(path) as (stream, _info):
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f'Oversized capacity input: {path}')
    return json.loads(data)


def atomic_json(path, value):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as stream:
        temporary = Path(stream.name)
        json.dump(value, stream, allow_nan=False)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    try:
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def set_limit(queue, limit, now):
    row = {'version': 1, 'limit': limit}
    if limit == 4:
        row['expires_unix'] = now + HEARTBEAT_SECONDS
    with queue.locked(time.monotonic() + 5):
        atomic_json(queue.directory / 'capacity.json', row)
        tickets, _ = queue.state()
        queue.notify(tickets)


def process_gate(process, queue, now, monotonic):
    """Only real gate processes bearing this host's heavy claim are observed."""
    try:
        raw_stat = (process / 'stat').read_text()
        fields = raw_stat.rsplit(')', 1)[1].split()
        if len(raw_stat) > 8192 or fields[0] == 'Z':
            return None
        with (process / 'cmdline').open('rb') as stream:
            words = stream.read(32769).split(b'\0')
        if sum(map(len, words)) > 32768:
            return None
        index = next((i for i in (1, 2) if len(words) > i + 1
                      and Path(os.fsdecode(words[i])).name == 'gate.py'), None)
        if index is None or words[index + 1] not in (b'full', b'affected'):
            return None
        with (process / 'environ').open('rb') as stream:
            environment = stream.read(MAX_BYTES + 1)
        if len(environment) > MAX_BYTES:
            raise ValueError('Oversized gate environment')
        # Retain only build metadata, never the rest of the process environment.
        selected = {key: value for item in environment.split(b'\0')
                    for key, _, value in [item.partition(b'=')]
                    if key in (b'SECODE_HEAVY_LEASE', b'CARGO_TARGET_DIR')}
        claim = json.loads(selected.get(b'SECODE_HEAVY_LEASE', b'{}'))
        if (claim.get('queue') != str(queue.directory.resolve())
                or claim.get('heavy') != str(queue.heavy.resolve())):
            return None
        root = (process / 'cwd').resolve(strict=True)
        arguments = [os.fsdecode(word) for word in words[index + 2:] if word]
        if '--list' in arguments:
            return None
        if '--log' in arguments:
            log = Path(arguments[arguments.index('--log') + 1])
            if not log.is_absolute():
                log = root / log
        else:
            target = os.fsdecode(selected[b'CARGO_TARGET_DIR'])
            log = Path(target) / 'gate-logs' / f'{process.name}.log'
        diagnostics = os.readlink(process / 'fd/2')
        info = os.stat(process / 'fd/2')
        regular_diagnostics = diagnostics.startswith('/') and stat.S_ISREG(info.st_mode)
        started = int(fields[19]) / os.sysconf('SC_CLK_TCK')
        execution_limit = (int(arguments[arguments.index('--timeout') + 1])
                           if '--timeout' in arguments else 10800 if words[index + 1] == b'full' else 1800)
        return {'id': f"{claim['ticket']}:{os.fsdecode(words[index + 1])}:{log}",
                'pid': int(process.name), 'process_start': fields[19],
                'execution_limit': execution_limit,
                'mode': os.fsdecode(words[index + 1]), 'log': str(log),
                'diagnostics': diagnostics if regular_diagnostics else None,
                'diagnostics_offset': info.st_size if regular_diagnostics else 0,
                'diagnostics_identity': [info.st_dev, info.st_ino] if regular_diagnostics else None,
                'started_unix': now - (monotonic - started), 'started_monotonic': started}
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return None


def discover(queue, now, monotonic, proc=Path('/proc')):
    result = {}
    count = 0
    for path in proc.iterdir():
        if not path.name.isdigit():
            continue
        count += 1
        if count > MAX_PROCESSES:
            raise ValueError('Capacity process census exceeds its bound')
        gate = process_gate(path, queue, now, monotonic)
        if gate:
            previous = result.get(gate['id'])
            # The RAM supervisor and its scoped child describe one gate.
            if previous is None or gate['started_monotonic'] < previous['started_monotonic']:
                result[gate['id']] = gate
            if len(result) > MAX_ACTIVE:
                raise ValueError('Too many simultaneous gates')
    return result


def tail(path, offset=0, identity=None):
    if path is None:
        return ''
    try:
        with regular_input(path) as (stream, info):
            if identity is not None and [info.st_dev, info.st_ino] != identity:
                raise ValueError('Gate diagnostics file was replaced during observation')
            if info.st_size < offset:
                raise ValueError('Gate diagnostics file was truncated during observation')
            stream.seek(max(offset, info.st_size - 65536))
            return stream.read(65536).decode(errors='replace')
    except FileNotFoundError:
        return ''


def completion(gate, now, monotonic):
    row = {key: value for key, value in gate.items() if key != 'started_monotonic'}
    row.update(finished_unix=now, seconds=monotonic - gate['started_monotonic'],
               duration_source='process lifetime', outcome='unknown')
    try:
        receipt = read_json(Path(gate['log'] + '.receipt.json'), 65536)
        if (receipt.get('schema') == 'secode.gate.v1' and receipt.get('mode') == gate['mode']
                and gate['started_unix'] <= receipt.get('finished_unix', 0) <= now + 5
                and type(receipt.get('seconds')) in (int, float)
                and 0 < receipt['seconds'] < float('inf')):
            row.update(seconds=receipt['seconds'], duration_source='gate receipt', outcome='pass')
    except FileNotFoundError:
        pass
    diagnostics = tail(gate['diagnostics'], gate.get('diagnostics_offset', 0),
                       gate.get('diagnostics_identity'))
    failed = re.findall(r'FAIL ' + gate['mode'] + r':[^\n]*\(([0-9.]+)s\)', diagnostics)
    if row['outcome'] != 'pass' and failed and float(failed[-1]) <= row['seconds'] + 5:
        row.update(seconds=float(failed[-1]), duration_source='gate failure', outcome='failed')
    if (row['outcome'] == 'failed' and row['seconds'] >= gate['execution_limit'] - 2
            and re.search(r'Gate timed out|Gate exceeded its time limit|gate execution.*timed out', diagnostics)):
        row['outcome'] = 'timed-out'
    return row


def verdict(baseline, fulls, row):
    if row['outcome'] == 'timed-out' and ((row['max_holders'] or 0) >= 4 or row.get('uncertain_samples', 0)):
        return {'action': 'rollback', 'reason': 'gate timed out with four holders or uncertain occupancy; causal attribution remains uncertain'}
    if len(fulls) == 6:
        old = statistics.median(item['seconds'] for item in baseline)
        new = statistics.median(item['seconds'] for item in fulls)
        return {'action': 'rollback' if new > old * 1.25 else 'keep-four',
                'reason': 'six full gates completed', 'baseline_full_count': len(baseline),
                'baseline_median_seconds': old, 'four_slot_median_seconds': new,
                'duration_ratio': new / old}
    return None


def append_history(directory, row):
    path = directory / 'gates.jsonl'
    if path.exists() and path.stat().st_size > 4 * MAX_BYTES:
        os.replace(path, directory / 'gates.previous.jsonl')
    with path.open('a') as stream:
        stream.write(json.dumps(row, allow_nan=False) + '\n')


def occupancy(queue):
    with queue.locked(time.monotonic() + 5):
        tickets, runs = queue.state()
        with queue.heavy.open('r') as handle:
            try:
                return queue.shared_locks(handle, {row['id'] for row in runs.holders(tickets)})
            except BlockingIOError:
                return None  # An unstable global inventory is not a measured count.


def observe(queue, directory, baseline, hours):
    import fcntl
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    with (directory / 'observer.lock').open('a') as guard:
        fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)
        armed = time.time()
        state = {'armed_unix': armed, 'baseline': baseline, 'fulls': [],
                 'comparison': None, 'rolled_back': False, 'active': {}}
        # Refuse to replace an armed experiment; a resume needs its original sample.
        state_path = directory / 'state.json'
        if state_path.exists():
            raise ValueError('Capacity observation already exists; preserve its original baseline')
        atomic_json(state_path, state)
        deadline = time.monotonic() + hours * 3600
        try:
            while time.monotonic() < deadline:
                now, monotonic = time.time(), time.monotonic()
                set_limit(queue, 3 if state['rolled_back'] else 4, now)
                holders = occupancy(queue)
                current = discover(queue, now, monotonic)
                for key, gate in current.items():
                    if key not in state['active']:
                        state['active'][key] = dict(gate, min_holders=holders,
                            max_holders=holders, samples=0,
                            uncertain_samples=0,
                            configured_limit=3 if state['rolled_back'] else 4)
                    active = state['active'][key]
                    if holders is None:
                        active['uncertain_samples'] += 1
                    else:
                        low, high = active['min_holders'], active['max_holders']
                        active['min_holders'] = holders if low is None else min(low, holders)
                        active['max_holders'] = holders if high is None else max(high, holders)
                    active['samples'] += 1
                for key in list(state['active']):
                    if key in current:
                        continue
                    row = completion(state['active'].pop(key), now, monotonic)
                    append_history(directory, row)
                    if (row['mode'] == 'full' and row['started_unix'] >= armed
                            and row['configured_limit'] == 4 and row['outcome'] == 'pass'
                            and len(state['fulls']) < 6):
                        state['fulls'].append(row)
                    result = verdict(baseline, state['fulls'], row)
                    if result and (state['comparison'] is None or
                                   result['action'] == 'rollback' and not state['rolled_back']):
                        result['finished_unix'] = now
                        state['comparison'] = result
                        if result['action'] == 'rollback':
                            state['rolled_back'] = True
                            set_limit(queue, 3, now)
                        atomic_json(directory / 'report.json', result)
                atomic_json(state_path, state)
                time.sleep(5)
        finally:
            set_limit(queue, 3, time.time())
            atomic_json(directory / 'monitor-ended.json', {'reason': 'observer stopped; admission returned to three',
                        'finished_unix': time.time()})


def main():
    import heavy
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--hours', type=int, default=168, choices=range(1, 169))
    args = parser.parse_args()
    baseline = read_json(args.baseline)
    if (not isinstance(baseline, list) or not baseline or len(baseline) > 128
            or any(type(row.get('seconds')) not in (int, float)
                   or not 0 < row['seconds'] < float('inf') for row in baseline)):
        raise ValueError('A finite nonempty full-gate baseline is required')
    observe(heavy.Queue(heavy.QUEUE_PATH, heavy.HEAVY_LOCK), args.directory, baseline, args.hours)


if __name__ == '__main__':
    def interrupted(_signal, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    main()
