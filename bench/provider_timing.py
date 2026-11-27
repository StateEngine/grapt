"""Bounded transport timing observations; no wire-arrival or fsync attribution."""
import argparse
import base64
from contextlib import closing
import json
from pathlib import Path
import re
import sqlite3

MAX_LOG = 32 * 1024 * 1024
MAX_CALLS = 256
PREFIX = re.compile(r'^GRAPT_PROVIDER_DISPATCH attempt=([A-Za-z0-9_-]{1,128}) at_ms=(-?\d+)(?:\s|$)')
PHASES = ('response_head_at_ms', 'first_stream_byte_at_ms')
OPEN_KINDS = ('session.assistant-message.opened.1', 'session.assistant-message.opened.2',
              'session.compaction.call.started.1')


def stamp(value):
    if value is None or value == 'null':
        return None
    if isinstance(value, str) and re.fullmatch(r'-?\d+', value):
        value = int(value)
    if type(value) is not int or not -(2**63) <= value < 2**63:
        raise ValueError('Invalid provider timestamp')
    return value


def interval(start, end):
    return end-start if start is not None and end is not None and end >= start else None


def diagnostic_lines(log):
    path = Path(log)
    if path.is_symlink() or path.stat().st_size > MAX_LOG:
        raise ValueError('Provider timing log is irregular or excessive')
    with path.open() as source:
        for line in source:
            try:
                row = json.loads(line)
            except ValueError:
                row = None
            if isinstance(row, dict) and 'raw_base64' in row:
                line = base64.b64decode(row['raw_base64'], validate=True).decode('utf-8')
            yield from line.splitlines()


def diagnostics(log):
    samples, loss = {}, None
    for diagnostic in diagnostic_lines(log):
        match = PREFIX.match(diagnostic)
        if match:
            attempt, sent = match.groups()
            if attempt in samples or len(samples) >= MAX_CALLS:
                raise ValueError('Duplicate or excessive provider timing samples')
            fields = dict(re.findall(r'\b(response_head_at_ms|first_stream_byte_at_ms|http_status)=([^\s]+)', diagnostic))
            sample = {'attempt': attempt, 'dispatch_at_ms': stamp(sent),
                      **{key: stamp(fields.get(key)) for key in PHASES}}
            status = stamp(fields.get('http_status'))
            if status is not None and not 100 <= status <= 599:
                raise ValueError('Invalid provider HTTP status')
            sample['http_status'] = status
            samples[attempt] = sample
        elif diagnostic.startswith('GRAPT_PROVIDER_DISPATCH_LOSS '):
            match = re.search(r'\bcount_at_least=(\d+)', diagnostic)
            if match is None:
                raise ValueError('Invalid provider timing loss count')
            count = int(match[1])
            if count > 2**64-1:
                raise ValueError('Excessive provider timing loss count')
            loss = max(loss or 0, count)
    return samples, loss


def openings(database, session):
    path = Path(database)
    if not isinstance(session, str) or not re.fullmatch(r'ses_[A-Za-z0-9_]{1,124}', session):
        raise ValueError('Timing session identity absent or invalid')
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 512 * 1024 * 1024:
        raise ValueError('Timing store absent, irregular or excessive')
    ticks = 0

    def progress():
        nonlocal ticks
        ticks += 1
        return ticks > 1000

    result = {}
    with closing(sqlite3.connect(path.absolute().as_uri()+'?mode=ro', uri=True, timeout=1)) as connection:
        connection.set_progress_handler(progress, 1000)
        connection.execute('PRAGMA query_only = ON')
        rows = connection.execute(
            "SELECT created, CASE WHEN length(CAST(data AS BLOB)) <= 65536 THEN data END "
            "FROM event WHERE aggregate_family = 'session' AND aggregate_id = ? "
            "AND type IN (?, ?, ?) ORDER BY seq LIMIT ?", (session, *OPEN_KINDS, MAX_CALLS+1))
        for created, raw in rows:
            if len(result) >= MAX_CALLS or not isinstance(raw, str):
                raise ValueError('Timing opening row bound exceeded or payload absent')
            attempt = json.loads(raw).get('attempt')
            if not isinstance(attempt, str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', attempt):
                raise ValueError('Timing opening identity absent or invalid')
            if attempt in result:
                raise ValueError('Duplicate timing opening')
            result[attempt] = stamp(created)
    if not result:
        raise ValueError('Timing opening evidence absent')
    return result


def collect(log, database, session):
    """Telemetry failure stays explicit and does not change coding outcomes."""
    try:
        samples, loss = diagnostics(log)
        opened = openings(database, session)
        calls = []
        for attempt, opening in opened.items():
            sample = samples.get(attempt, dict.fromkeys(('dispatch_at_ms', *PHASES, 'http_status')))
            sent, head, body = (sample[key] for key in ('dispatch_at_ms', *PHASES))
            ordered = (sent is not None and (head is None or head >= sent)
                       and (body is None or head is not None and body >= head))
            calls.append({'attempt': attempt, 'opening_created_at_ms': opening, **sample,
                          'dispatch_prep_ms': interval(opening, sent),
                          'response_head_wait_ms': interval(sent, head) if ordered else None,
                          'provider_ttfb_ms': interval(sent, body) if ordered else None,
                          'head_to_first_stream_byte_ms': interval(head, body) if ordered else None,
                          'transport_stamps_ordered': ordered if sent is not None else None})
        return {'provider_timing': {'status': 'observed', 'calls': calls,
                'opened_attempts': len(opened),
                'dispatch_samples': sum(a in samples for a in opened),
                'ttfb_samples': sum(c['provider_ttfb_ms'] is not None for c in calls),
                'diagnostic_loss_at_least': loss,
                'limits': ['dispatch_prep_ms starts at opening event creation before its commit; '
                           'it includes commit and preparation, not a pure preparation CPU measurement.',
                           'provider_ttfb_ms ends at the first nonempty response chunk observed '
                           'before decoding; it includes transport polling and scheduling delays.',
                           'Missing phases and inverted timestamps remain unknown; '
                           'refusal bodies are not stream observations.']}}
    except (OSError, sqlite3.Error, ValueError, TypeError, AttributeError) as error:
        return {'provider_timing': {'status': 'unavailable', 'calls': None, 'reason': str(error)}}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--log', required=True, type=Path)
    parser.add_argument('--database', required=True, type=Path)
    parser.add_argument('--session', required=True)
    args = parser.parse_args()
    result = collect(args.log, args.database, args.session)
    print(json.dumps(result, indent=2))
    raise SystemExit(0 if result['provider_timing']['status'] == 'observed' else 1)
