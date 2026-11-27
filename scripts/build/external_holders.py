"""Measured admission charges for live shared holders outside the ticket queue."""
import math
import os
from pathlib import Path

import heavy_sampling


def slot_charge(count, limit):
    return min(count, max(1, limit // 2))


def charged_count(count, external, limit):
    return count - external['count'] + slot_charge(external['count'], limit) if external else count


def cpu_ticks():
    with Path('/proc/stat').open('rb') as stream:
        fields = stream.readline(4097).split()
    if len(fields) < 9 or fields[0] != b'cpu' or len(fields) > 32:
        raise ValueError('Invalid bounded host CPU sample')
    values = [int(value) for value in fields[1:9]]
    return [sum(values) - values[3] - values[4], sum(values)]


def ancestor(pid, roots, forest):
    seen = set()
    while pid in forest and pid not in seen:
        if pid in roots:
            return True
        seen.add(pid)
        pid = forest[pid]['parent']
    return False


def measure(pids, governed, forest, host, limit, previous, now, counters):
    if len(pids) > 1024:
        raise ValueError('External holder inventory exceeds its bound')
    if previous is not None:
        if (not isinstance(previous, dict) or set(previous) != {'at', 'roots', 'ticks', 'busy'}
                or not isinstance(previous['roots'], dict) or len(previous['roots']) > 1024
                or any(type(previous[key]) not in (int, float) or not math.isfinite(previous[key])
                       or previous[key] < 0 for key in ('at', 'busy'))
                or not isinstance(previous['ticks'], list) or len(previous['ticks']) != 2
                or any(type(value) is not int or value < 0 for value in previous['ticks'])):
            raise ValueError('Invalid external holder history')
        for key, row in previous['roots'].items():
            if (not isinstance(key, str) or len(key) > 64 or not isinstance(row, list) or len(row) != 2
                    or any(type(value) not in (int, float) or not math.isfinite(value) or value < 0 for value in row)):
                raise ValueError('Invalid external process history')
        if now < previous['at'] or counters[1] < previous['ticks'][1]:
            previous = None  # A machine reboot resets these kernel counters.
    live = {pid for pid in pids if pid in forest and not ancestor(pid, governed, forest)}
    roots = {pid for pid in live if not ancestor(forest[pid]['parent'], live, forest)}
    elapsed = now - previous['at'] if previous else 0
    fresh = previous is None or elapsed >= 1
    old_roots = previous['roots'] if previous else {}
    samples = {}
    rss = cpu = 0
    frequency = os.sysconf('SC_CLK_TCK')
    for pid in roots:
        # Live governed subtrees have their own reservations. Linux includes
        # reaped children in an ancestor's cumulative CPU ticks; those historic
        # ticks cannot be separated and may conservatively affect one delta.
        sample = heavy_sampling.family(pid, forest, excluded=governed)
        key = f"{pid}:{forest[pid]['start']}"
        old = old_roots.get(key)
        if old is not None and elapsed >= 1 and sample['ticks'] >= old[0]:
            used = (sample['ticks'] - old[0]) / frequency / elapsed
        elif old is not None and elapsed >= 0:
            used = old[1]
        else:
            age = max(1 / frequency, now - int(forest[pid]['start']) / frequency)
            used = sample['ticks'] / frequency / age
        rss += sample['rss']
        cpu += used
        samples[key] = [sample['ticks'], used]
    busy = previous['busy'] if previous else min(host['load'], host['cores'])
    if previous and elapsed >= 1:
        delta = counters[1] - previous['ticks'][1]
        active = counters[0] - previous['ticks'][0]
        if delta <= 0 or not 0 <= active <= delta:
            raise ValueError('Host CPU counters did not advance consistently')
        busy = host['cores'] * active / delta
    charged_cpu = min(cpu, host['cores'] / 2)
    charge = dict(count=len(live), slots=slot_charge(len(live), limit),
                  peer=dict(rss=rss, peak_rss=min(rss * 1.25, host.get('total', host['available']) / 2), cpu=charged_cpu),
                  # Load average includes I/O wait. Preserve other measured CPU
                  # load while applying the explicit external CPU share cap.
                  load=max(0, busy - max(0, cpu - charged_cpu)), cpu=cpu, rss=rss,
                  warmup=previous is None)
    history = dict(at=now, roots=samples, ticks=counters, busy=busy) if fresh else previous
    return charge, history
