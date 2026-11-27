"""Bounded Linux IO admission before a benchmark starts any model work."""
from datetime import datetime, timezone
import math
from pathlib import Path
import time


def read():
    text = Path('/proc/pressure/io').read_text()
    rows = [line.split() for line in text.splitlines() if line.startswith('some ')]
    if len(rows) != 1:
        raise ValueError('IO pressure must contain one some row')
    fields = dict(item.split('=', 1) for item in rows[0][1:])
    value = float(fields['avg10'])
    if not math.isfinite(value) or not 0 <= value <= 100:
        raise ValueError('Invalid IO pressure avg10')
    return {'received_at': datetime.now(timezone.utc).isoformat(),
            'some_avg10_percent': value}


def wait(threshold, timeout):
    if not math.isfinite(threshold) or not 0 < threshold <= 100 or not 0 < timeout <= 900:
        raise ValueError('Invalid IO pressure admission bounds')
    began = time.monotonic()
    while True:
        sample = read()
        elapsed = time.monotonic() - began
        if sample['some_avg10_percent'] < threshold:
            return dict(sample, threshold_percent=threshold, wait_seconds=elapsed)
        if elapsed >= timeout:
            raise RuntimeError(f'IO capacity unavailable after {timeout}s: {sample}')
        time.sleep(min(5, timeout-elapsed))
