"""Hardware admission and bounded observations beside legacy-compatible Runs."""
import json
import math
import os
from pathlib import Path
import re
import time
from heavy_runs import metadata, MAX_RECENT
import heavy_sampling

GIB = 1024**3
SANITY_LIMIT = 12
RAM_RESERVE = 16 * GIB
DISK_FLOOR = 60_000_000_000
DEFAULT_RSS = 8 * GIB
UNKNOWN_LEASE_RSS = 24 * GIB
DEFAULT_CPU = 8
MAX_BYTES = 1024 * 1024
PRESSURE_LIMITS = (('cpu', 25), ('memory', 1), ('io', 15))


def finite(value):
    return type(value) in (int, float) and math.isfinite(value) and value >= 0


def read(path, default):
    try:
        with metadata(path, os.O_RDONLY, 'rb') as stream:
            raw = stream.read(MAX_BYTES+1)
    except FileNotFoundError:
        return default
    if len(raw) > MAX_BYTES:
        raise ValueError('Hardware metadata exceeds its bound')
    return json.loads(raw)


def write(path, value):
    raw = json.dumps(value, allow_nan=False, separators=(',', ':'))
    if len(raw.encode()) > MAX_BYTES:
        raise ValueError('Hardware metadata exceeds its bound')
    staging = path.with_suffix('.next')
    with metadata(staging, os.O_CREAT | os.O_TRUNC | os.O_WRONLY, 'w') as stream:
        stream.write(raw)
        stream.flush()
    staging.replace(path)


def ceiling(directory, *, legacy=False):
    filename = 'hardware-ceiling.json' if legacy else 'hardware-ceiling-v2.json'
    row = read(Path(directory)/filename, {'version': 1, 'limit': SANITY_LIMIT, 'reason': 'hardware'})
    if (not isinstance(row, dict) or set(row) != {'version', 'limit', 'reason'}
            or row['version'] != 1 or type(row['limit']) is not int
            or not 1 <= row['limit'] <= SANITY_LIMIT or not isinstance(row['reason'], str)
            or len(row['reason']) > 1024):
        raise ValueError('Invalid hardware safety ceiling')
    return row['limit']


def set_ceiling(directory, limit, reason):
    if type(limit) is not int or not 1 <= limit <= SANITY_LIMIT:
        raise ValueError('Hardware safety ceiling must be between one and twelve')
    row = {'version': 1, 'limit': limit, 'reason': reason}
    write(Path(directory)/'hardware-ceiling.json', row)
    write(Path(directory)/'hardware-ceiling-v2.json', row)


def refusal(host, candidate, peers, count, limit=SANITY_LIMIT, *, weighted_cpu=False):
    """Current RSS/tmpfs is already in MemAvailable; reserve only future growth."""
    if any(not finite(host[key]) for key in ('available', 'cores', 'load', 'nvme_free', 'root_free')):
        raise ValueError('Invalid hardware sample')
    if type(weighted_cpu) is not bool:
        raise ValueError('Invalid CPU priority sample')
    if count >= limit:
        return 'safety ceiling'
    future = sum(max(0, peer['peak_rss'] - peer['rss']) for peer in peers)
    if host['available'] < RAM_RESERVE + future + candidate['peak_rss']:
        return 'available RAM and projected growth'
    pressure = host.get('pressure')
    if pressure is not None:
        if (not isinstance(pressure, dict) or set(pressure) != {'cpu', 'memory', 'io'}
                or any(not finite(value) or value > 100 for value in pressure.values())):
            raise ValueError('Invalid hardware pressure sample')
        for resource, threshold in PRESSURE_LIMITS:
            if pressure[resource] >= threshold:
                return f'measured {resource} pressure'
    cpu_limit = max(1, host['cores'] - max(1, math.ceil(host['cores']/12)))
    if not weighted_cpu and sum(peer['cpu'] for peer in peers) + candidate['cpu'] > cpu_limit:
        return 'projected CPU'
    if not weighted_cpu and host['load'] + candidate['cpu'] > cpu_limit:
        return 'load guard'
    if not candidate.get('maintenance', False) and (
            host['nvme_free'] < DISK_FLOOR
            or candidate.get('root_writes', True) and host['root_free'] < DISK_FLOOR):
        return 'disk free-space floor'
    return None


def kind(owner):
    match = re.search(r'(?:^|[-_.:])(full|nightly|affected|composition|gate|echo|idle)(?:[-_.:]|$)', owner, re.I)
    return ('full' if match[1].lower() == 'nightly' else match[1].lower()) if match else 'other'


def uses_ram(allocation):
    # Older NVMe wrappers could start nested RAM gates without changing slot.
    return allocation.get('slot') == 'ram' or 'ram' in allocation.get('targets', ())


class Profiles:
    """Queue.locked owns writes; v1 clients ignore this sibling resource history."""
    def __init__(self, directory, tickets, runs):
        self.directory = Path(directory)
        self.path = self.directory/'hardware-runs.json'
        self.value = read(self.path, {'version': 1, 'clients': {}, 'active': {}, 'recent': [], 'observed': 0})
        v = self.value
        if (not isinstance(v, dict) or set(v) != {'version', 'clients', 'active', 'recent', 'observed'}
                or v['version'] != 1 or not finite(v['observed'])
                or any(not isinstance(v[key], dict) or len(v[key]) > 1024 for key in ('clients', 'active'))
                or not isinstance(v['recent'], list) or len(v['recent']) > MAX_RECENT):
            raise ValueError('Invalid hardware run history')
        for key, row in v['clients'].items():
            if (not re.fullmatch(r'[0-9-]{1,80}', key) or not isinstance(row, dict)
                    or set(row) != {'jobs', 'log'} or type(row['jobs']) is not int
                    or not 1 <= row['jobs'] <= 16 or row['log'] is not None and (
                        not isinstance(row['log'], str) or len(row['log']) > 4096 or not Path(row['log']).is_absolute())):
                raise ValueError('Invalid hardware client')
        for row in list(v['active'].values()) + v['recent']:
            if (not isinstance(row, dict) or set(row) != {'id', 'owner', 'rss', 'peak_rss', 'ticks', 'cpu', 'peak_cpu', 'at', 'since', 'holders', 'seconds', 'exit'}
                    or not isinstance(row['id'], str) or not re.fullmatch(r'[0-9-]{1,80}', row['id'])
                    or not isinstance(row['owner'], str) or not re.fullmatch(r'[a-zA-Z0-9_.:@-]{1,128}', row['owner'])
                    or any(not finite(row[key]) for key in ('rss', 'peak_rss', 'ticks', 'cpu', 'peak_cpu', 'at', 'since', 'holders'))
                    or row['seconds'] is not None and not finite(row['seconds'])
                    or row['exit'] is not None and type(row['exit']) is not int):
                raise ValueError('Invalid hardware observation')
        if any(key != row['id'] for key, row in v['active'].items()):
            raise ValueError('Hardware observation identity mismatch')
        self.tickets, self.runs = tickets, runs
        live = {t['id'] for t in tickets}
        v['clients'] = {key: row for key, row in v['clients'].items() if key in live}
        v['active'] = {key: row for key, row in v['active'].items() if key in live}

    def save(self):
        write(self.path, self.value)

    def register(self, ticket, jobs, log):
        if type(jobs) is not int or not 1 <= jobs <= 16:
            raise ValueError('Hardware jobs must be between one and sixteen')
        if log is not None and (not Path(log).is_absolute() or len(str(log)) > 4096):
            raise ValueError('Hardware gate log must be a bounded absolute path')
        self.value['clients'][ticket['id']] = {'jobs': jobs, 'log': str(log) if log else None}
        self.save()

    def capable(self, ticket):
        return ticket['id'] in self.value['clients']

    def projection(self, ticket, ram=False):
        # Runs owns successful completion; never learn a low peak from a failed run.
        successful = {row['id'] for row in self.runs.recent if row['exit'] == 0}
        measured = [row for row in self.value['recent'] if row['id'] in successful]
        same = [row for row in measured if row['owner'] == ticket['owner']]
        selected = same or [row for row in measured if kind(ticket['owner']) != 'other'
                           and kind(row['owner']) == kind(ticket['owner'])]
        jobs = self.value['clients'].get(ticket['id'], {}).get('jobs', DEFAULT_CPU)
        # Either filesystem can need recovery after enqueue chose normal priority.
        maintenance = ticket['owner'] == 'disk-hygiene'
        rss = max(row['peak_rss'] for row in selected[-31:]) * 1.25 if selected else DEFAULT_RSS
        cpu = max(row['peak_cpu'] for row in selected[-31:]) * 1.25 if selected else jobs
        failed = [row for row in self.value['recent'] if row['exit'] not in (0, None)
                  and (row['owner'] == ticket['owner'] or kind(ticket['owner']) != 'other'
                       and kind(row['owner']) == kind(ticket['owner']))]
        rss = max([rss] + [row['peak_rss'] * 1.25 for row in failed[-31:]])
        cpu = max([cpu] + [row['peak_cpu'] * 1.25 for row in failed[-31:]])
        if maintenance:
            rss, cpu = 256*1024**2, 1
        return dict(peak_rss=24*GIB if ram else max(128*1024**2, rss),
                    cpu=min(jobs, max(1, cpu)), maintenance=maintenance)

    def placement(self, ticket, host, peers, count, limit, *, ram, shared):
        """Use the same RAM-first, verified-disk fallback for admission and status."""
        def check(use_ram):
            candidate = dict(self.projection(ticket, use_ram), root_writes=not use_ram and not shared)
            return refusal(host, candidate, peers, count, limit,
                           weighted_cpu=host.get('weighted_cpu', False))
        if ram:
            reason = check(True)
            if reason is None or not shared:
                return True, reason
        return False, check(False)

    def observe(self, forest=None):
        now = time.monotonic()
        if 0 <= now-self.value['observed'] < 1:
            return
        forest = heavy_sampling.processes() if forest is None else forest
        holders = self.runs.holders(self.tickets)
        for ticket in holders:
            root = forest.get(ticket['pid'])
            if root is None or ticket['start'].rsplit(':', 1)[-1] != root['start']:
                continue
            sample = heavy_sampling.family(ticket['pid'], forest)
            allocation = self.runs.active.get(ticket['id'], {})
            ram = heavy_sampling.ram_current(allocation)
            if uses_ram(allocation):
                sample['rss'] = ram if ram is not None else 0
            row = self.value['active'].get(ticket['id'])
            projected = self.projection(ticket, uses_ram(allocation))
            cpu = projected['cpu']
            if row is not None and now > row['at']:
                delta = sample['ticks']-row['ticks']
                if delta >= 0:
                    cpu = max(1, delta/os.sysconf('SC_CLK_TCK')/(now-row['at']) * 1.5)
            previous = row or {}
            self.value['active'][ticket['id']] = dict(id=ticket['id'], owner=ticket['owner'],
                rss=sample['rss'], peak_rss=max(sample['rss'], previous.get('peak_rss', 0)),
                ticks=sample['ticks'], cpu=min(16, cpu), peak_cpu=max(cpu, previous.get('peak_cpu', 0)),
                at=now, since=previous.get('since', time.time()), holders=max(len(holders), previous.get('holders', 0)), seconds=None, exit=None)
        self.value['observed'] = now
        self.save()

    def external(self, pids, holders, host, limit, forest=None):
        import external_holders
        path = self.directory / 'external-holders.json'
        previous = read(path, None)
        forest = heavy_sampling.processes() if forest is None else forest
        charge, history = external_holders.measure(pids, {row['pid'] for row in holders},
            forest, host, limit, previous, time.monotonic(), external_holders.cpu_ticks())
        if history is not None and history is not previous:
            write(path, history)
            if charge:
                import sys
                peer = charge['peer']
                print(f"External shared holders: {charge['count']} measured, "
                      f"RSS={charge['rss']/GIB:.2f}GiB CPU={charge['cpu']:.2f}; "
                      f"charged peak={peer['peak_rss']/GIB:.2f}GiB CPU={peer['cpu']:.2f} "
                      f"slots={charge['slots']}; 50% hardware share cap; "
                      f"measured host CPU={history['busy']:.2f}, warmup={charge['warmup']}", file=sys.stderr, flush=True)
        return charge

    def peers(self, holders, count, external=None):
        peers = []
        for ticket in holders:
            allocation = self.runs.active.get(ticket['id'], {})
            projected = self.projection(ticket, uses_ram(allocation))
            observed = self.value['active'].get(ticket['id'], {})
            # A legacy nested scope has no verified governor in the lease.
            # Its process-family RSS cannot credit materialized tmpfs pages.
            untracked_ram = uses_ram(allocation) and allocation.get('slot') != 'ram'
            peers.append(dict(peak_rss=max(projected['peak_rss'], observed.get('peak_rss', 0)),
                              rss=0 if untracked_ram else observed.get('rss', 0),
                              cpu=observed.get('cpu', projected['cpu'])))
        # A lease can outlive its wrapper and queue row. Its placement is then
        # unknown: it may still hold the full RAM-governed reservation. Do not
        # infer a smaller disk profile or credit unverified materialized pages.
        missing = max(0, count - len(holders))
        if external is not None:
            if not 0 <= external['count'] <= missing:
                raise ValueError('External lock sample differs from the shared lease count')
            missing -= external['count']
            peers.append(external['peer'])
        peers.extend(dict(peak_rss=UNKNOWN_LEASE_RSS, rss=0, cpu=DEFAULT_CPU) for _ in range(missing))
        return peers

    def finish(self, ticket, code):
        row = self.value['active'].pop(ticket['id'], None)
        client = self.value['clients'].pop(ticket['id'], {})
        if row is None:
            self.save()
            return
        row['exit'] = code
        log = client.get('log')
        if code == 0 and log:
            receipt = read(Path(log+'.receipt.json'), {})
            source = receipt.get('source_after', {})
            if (receipt.get('schema') == 'secode.gate.v1' and receipt.get('mode') == 'full'
                    and receipt.get('exit') == 0 and source.get('clean') is True
                    and receipt.get('source_before') == source
                    and finite(receipt.get('seconds')) and receipt['seconds'] > 0
                    and finite(receipt.get('finished_unix')) and receipt['finished_unix'] >= row['since']):
                row['seconds'] = receipt['seconds']  # Gate excludes both queue and workspace waits.
        self.value['recent'] = (self.value['recent'] + [row])[-MAX_RECENT:]
        self.save()
