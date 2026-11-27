#!/usr/bin/env python3
"""Enrich a saved weekly report from public native logs without replacing old fields."""
import argparse
import copy
import hashlib
import json
from pathlib import Path

import report
import run

NEW_FIELDS = ('call_usage', 'cache_write_tokens_observed', 'cache_write_durations')


def pin(path):
    return {'path': str(path), 'bytes': path.stat().st_size,
            'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def enrich(original, native_root):
    document = copy.deepcopy(original)
    pins = []
    for row in document['attempts']:
        harness = row.get('native_harness', row['harness'])
        if harness not in {'codex', 'claude', 'opencode'}:
            continue
        # These identities come from our saved planned sets, never provider text.
        if any(Path(row[k]).name != row[k] or row[k] in {'.', '..'}
               for k in ['harness', 'task']):
            raise ValueError('Unsafe saved attempt identity')
        log = native_root/row['harness']/row['task']/'agent.jsonl'
        if log.is_symlink() or not log.is_file() or log.stat().st_size > run.MAX_LOG:
            raise ValueError('Native log absent, irregular or oversized')
        observed = run.telemetry(log, harness)
        if observed['usage'] != row.get('usage'):
            raise ValueError(f'Native usage differs from saved aggregate: {row["harness"]}/{row["task"]}')
        if any(field in row for field in NEW_FIELDS):
            raise ValueError('Saved report already contains replay fields')
        row.update({key: observed[key] for key in NEW_FIELDS if key in observed})
        pins.append(pin(log))
    calculated = report.summary(document['attempts'], document['planned_harnesses'],
                                document['planned_tasks'], document.get('adapters'))
    for harness, old in original['summary'].items():
        if any(calculated[harness].get(key) != value for key, value in old.items()):
            raise ValueError(f'Original summary changed: {harness}')
    for old, new in zip(original['attempts'], document['attempts']):
        if any(new.get(key) != value for key, value in old.items()):
            raise ValueError('Original attempt field changed')
    document['summary'] = calculated
    document['cache_metrics_replay'] = {'state': 'offline-original-fields-and-totals-preserved',
        'native_logs': pins, 'billing': 'No provider-reported per-row bills in these saved logs',
        'grapt': 'Historical usage remains unknown; no store backfill',
        'codex': 'Whole-run usage does not identify individual calls'}
    return document


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('original', type=Path)
    parser.add_argument('--native-root', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--markdown', required=True, type=Path)
    args = parser.parse_args()
    if any(path.exists() or path.is_symlink() for path in [args.output, args.markdown]):
        parser.error('Replay outputs must be new paths')
    try:
        document = enrich(json.loads(args.original.read_text()), args.native_root)
        document['cache_metrics_replay']['original'] = pin(args.original)
        document['cache_metrics_replay']['prices'] = pin(Path(__file__).with_name('cache-prices.json'))
        for path in [args.output, args.markdown]:
            path.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(document, indent=2)+'\n')
        args.markdown.write_text(report.markdown([document]))
    except (OSError, ValueError, TypeError) as error:
        parser.error(str(error))


if __name__ == '__main__':
    main()
