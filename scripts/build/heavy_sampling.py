"""Bounded Linux host/process samples; never retain command lines or secrets."""
from operator_paths import nvme_mount
import os
import math
from pathlib import Path
import shutil

MAX_PROCESSES = 32768
MAX_PROCESS_BYTES = 8192


def pressure(root=Path('/proc/pressure')):
    result = {}
    for resource, category in (('cpu', 'some'), ('memory', 'full'), ('io', 'full')):
        with (root/resource).open() as source:
            raw = source.read(4097)
        if len(raw) > 4096:
            raise ValueError('Pressure sample exceeds its bound')
        rows = [line.split() for line in raw.splitlines()]
        selected = [row for row in rows if row and row[0] == category]
        if len(selected) != 1:
            raise ValueError('Required pressure sample is unavailable')
        try:
            fields = dict(field.split('=', 1) for field in selected[0][1:])
            value = float(fields['avg10'])
        except (KeyError, ValueError) as error:
            raise ValueError('Invalid pressure average') from error
        if not math.isfinite(value) or not 0 <= value <= 100:
            raise ValueError('Invalid pressure average')
        result[resource] = value
    return result


def build_mount():
    selected = nvme_mount()
    if selected is None:
        raise ValueError('Configure SECODE_NVME_ROOT or a dedicated mounted SECODE_CACHE_DIR')
    return selected


def host():
    import cpu_priority
    fields = dict(line.split(':', 1) for line in Path('/proc/meminfo').read_text().splitlines())
    # MemAvailable already accounts for tmpfs. Subtracting Shmem again is wrong.
    available = int(fields['MemAvailable'].split()[0]) * 1024
    cores = len(os.sched_getaffinity(0))
    return dict(available=available, total=int(fields['MemTotal'].split()[0]) * 1024,
                cores=cores, load=os.getloadavg()[0],
                nvme_free=shutil.disk_usage(build_mount()).free,
                root_free=shutil.disk_usage('/').free, weighted_cpu=cpu_priority.configured(),
                pressure=pressure())


def processes(proc=Path('/proc')):
    result = {}
    for index, path in enumerate(p for p in proc.iterdir() if p.name.isdigit()):
        if index >= MAX_PROCESSES:
            raise ValueError('Hardware process inventory exceeds its bound')
        try:
            if path.stat().st_uid != os.getuid():
                continue
            with (path/'stat').open('rb') as source:
                raw = source.read(MAX_PROCESS_BYTES + 1)
            if len(raw) > MAX_PROCESS_BYTES:
                raise ValueError('Hardware process stat exceeds its bound')
            fields = raw.rsplit(b')', 1)[1].split()
            if len(fields) < 22 or fields[0] in (b'Z', b'X'):
                continue
            result[int(path.name)] = dict(parent=int(fields[1]), start=fields[19].decode(),
                ticks=sum(int(fields[i]) for i in (11, 12, 13, 14)),
                rss=max(0, int(fields[21])) * os.sysconf('SC_PAGE_SIZE'))
        except (FileNotFoundError, ProcessLookupError):
            continue
        except PermissionError:
            raise ValueError('A hardware process sample is unreadable') from None
    return result


def family(root, forest, excluded=()):
    """Sum a wrapper and descendants, including child CPU already reaped by it."""
    children = {}
    for pid, row in forest.items():
        children.setdefault(row['parent'], []).append(pid)
    pending, seen = [root], set()
    rss = ticks = 0
    while pending:
        pid = pending.pop()
        if pid in seen or pid in excluded:
            continue
        seen.add(pid)
        row = forest.get(pid)
        if row is not None:
            rss += row['rss']
            ticks += row['ticks']
        pending.extend(children.get(pid, []))
    return dict(rss=rss, ticks=ticks)


def ram_current(allocation):
    """Credit RAM pages already reflected in MemAvailable only from the governor."""
    row = allocation.get('ram_row')
    if allocation.get('slot') != 'ram' or row is None:
        return None
    unit = row['unit']
    if not unit.startswith('secode-ram-run-') or not unit.endswith('.scope') or '/' in unit:
        raise ValueError('Invalid RAM governor unit')
    import cpu_priority
    priority = cpu_priority.CGROUP / cpu_priority.SLICE / unit
    if allocation.get('path'):
        import ram_targets
        path = Path(allocation['path'])
        selected = cpu_priority.recorded_slice(path)
        if selected is not None:
            if ram_targets.read_run(path) != row:
                raise ValueError('RAM priority placement differs from its lease')
            if selected != cpu_priority.SLICE:
                priority = cpu_priority.CGROUP / cpu_priority.SLICE / selected / unit
    observed = []
    for group in (cpu_priority.CGROUP / 'app.slice' / unit, priority):
        try:
            maximum = int((group/'memory.max').read_text())
            current = int((group/'memory.current').read_text())
        except FileNotFoundError:
            if group.exists():
                raise ValueError('RAM governor accounting is incomplete') from None
            continue
        observed.append((maximum, current))
    if not observed:
        return None  # Not started yet: reserve the entire unmaterialized budget.
    if len(observed) != 1:
        raise ValueError('RAM governor exists in multiple slices')
    maximum, current = observed[0]
    if not 0 <= current or not 0 < maximum <= allocation['budget']:
        raise ValueError('RAM governor does not match its reservation')
    return current
