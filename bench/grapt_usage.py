"""Read one benchmark session's durable usage before its private store is removed."""
import hashlib
import json
from pathlib import Path
import re
import sqlite3

import grapt_bindings
import grapt_fork_usage

MAX_ROWS = 4096
MAX_ATTEMPTS = 256
MAX_ROW_BYTES = 65536
MAX_BYTES = 16 * 1024 * 1024
MAX_TOKENS = 2**53
FIELDS = ('inputTotalTokens', 'inputFreshTokens', 'outputTokens',
          'cacheReadInputTokens', 'cacheWriteInputTokens')
PHASES = {'during-stream': 0, 'terminal': 1, 'reconciled': 2}
PROVENANCE = {'harness-estimated': 0, 'provider-derived': 1, 'provider-reported': 2}


def merge(held, incoming):
    if held is None or incoming[0] > held[0]:
        return incoming
    if incoming[0] < held[0]:
        return held
    if incoming[2] != held[2]:
        raise ValueError('Conflicting usage at the same observation phase')
    return max(held, incoming, key=lambda value: value[1])


def read_evidence(database, session):
    path = Path(database)
    if not isinstance(session, str) or not re.fullmatch(r'ses_[A-Za-z0-9_]{1,124}', session):
        raise ValueError('Benchmark session identity is missing or invalid')
    if path.is_symlink() or not path.is_file():
        raise ValueError('Benchmark usage store is absent or irregular')
    if path.stat().st_size > 512 * 1024 * 1024:
        raise ValueError('Benchmark usage store exceeds 512 MiB')
    attempts, opened, summaries, observations, rows_read, bytes_read = {}, set(), set(), [], 0, 0
    opening_seq = {}
    messages, bindings, calls = set(), {}, {}
    connection = sqlite3.connect(path.absolute().as_uri()+'?mode=ro', uri=True, timeout=1)
    ticks = 0

    def progress():
        nonlocal ticks
        ticks += 1
        return ticks > 1000

    def decode(body):
        nonlocal rows_read, bytes_read
        rows_read += 1
        if rows_read > MAX_ROWS or not isinstance(body, str):
            raise ValueError('Usage event row bound exceeded or payload absent')
        bytes_read += len(body.encode())
        if bytes_read > MAX_BYTES:
            raise ValueError('Usage event byte bound exceeded')
        document = json.loads(body)
        if not isinstance(document, dict):
            raise ValueError('Usage event payload is not an object')
        return document

    try:
        connection.set_progress_handler(progress, 1000)
        connection.execute('PRAGMA query_only = ON')
        connection.execute('BEGIN')
        inherited, inherited_usage = grapt_fork_usage.inherited_attempts(
            connection, session, decode, MAX_ATTEMPTS, MAX_ROWS, MAX_ROW_BYTES)
        inherited_messages = set()
        rows = connection.execute(
            "SELECT seq, type, CASE WHEN length(CAST(data AS BLOB)) <= ? THEN data END "
            "FROM event WHERE aggregate_family = 'session' AND aggregate_id = ? "
            "AND (type LIKE 'session.usage.observed.%' "
            "OR type LIKE 'session.compaction.call.usage.%' "
            "OR type IN ('session.assistant-message.opened.1', 'session.assistant-message.opened.2', "
            "'session.compaction.call.started.1', 'session.content-block.opened.1', "
            "'session.content.appended.1', 'session.tool.terminal.1')) "
            "ORDER BY seq LIMIT ?", (MAX_ROW_BYTES, session, MAX_ROWS+1))
        for seq, kind, body in rows:
            document = decode(body)
            if kind == 'session.content.appended.1':
                message, call = grapt_bindings.appended(document)
                if message in inherited_messages:
                    continue
                if call is not None:
                    if message not in messages or len(call) > 128:
                        raise ValueError('Tool binding has no known assistant message or call')
                    if call in calls and calls[call] != message:
                        raise ValueError('Tool call belongs to multiple assistant messages')
                    calls[call] = message
                continue
            attempt = document.get('attempt')
            if not isinstance(attempt, str) or not 1 <= len(attempt) <= 128:
                raise ValueError('Usage attempt identity is missing or invalid')
            if attempt in inherited:
                if (kind.startswith('session.usage.observed.') and
                        grapt_fork_usage.fingerprint(document) not in inherited_usage.get((kind, attempt), set())):
                    raise ValueError('Inherited usage differs from the frozen parent evidence')
                if kind.startswith('session.assistant-message.opened.'):
                    inherited_messages.add(grapt_bindings.opened(document, int(kind.rsplit('.', 1)[1])))
                continue
            if kind in {'session.content-block.opened.1', 'session.tool.terminal.1'}:
                if kind == 'session.content-block.opened.1':
                    message = grapt_bindings.block(document)
                else:
                    # Only top-level calls appended to an assistant bind a retry.
                    # Nested CodeMode tool terminals belong to a different operation.
                    call = grapt_bindings.terminal(document)
                    if len(call) > 128:
                        raise ValueError('Tool terminal call binding is too long')
                    message = calls.get(call)
                    if message is None:
                        continue
                if (not isinstance(message, str) or message not in messages
                        or attempt in summaries
                        or (attempt in bindings and bindings[attempt] != message)):
                    raise ValueError('Retry belongs to no known assistant or to multiple messages')
                bindings[attempt] = message
                opened.add(attempt)
                opening_seq.setdefault(attempt, seq)
                if len(opened) > MAX_ATTEMPTS:
                    raise ValueError('Usage attempt bound exceeded')
                continue
            if kind.startswith('session.assistant-message.opened.') or kind == 'session.compaction.call.started.1':
                if attempt in opened:
                    raise ValueError('Physical Attempt was opened twice')
                if kind.startswith('session.assistant-message.opened.'):
                    message = grapt_bindings.opened(document, int(kind.rsplit('.', 1)[1]))
                    if len(message) > 128 or message in messages:
                        raise ValueError('Assistant message binding is invalid or repeated')
                    messages.add(message)
                    bindings[attempt] = message
                opened.add(attempt)
                opening_seq[attempt] = seq
                if kind == 'session.compaction.call.started.1':
                    summaries.add(attempt)
                if len(opened) > MAX_ATTEMPTS:
                    raise ValueError('Usage attempt bound exceeded')
                continue
            if kind not in {'session.usage.observed.2', 'session.compaction.call.usage.1'}:
                raise ValueError('Unsupported usage version; legacy input is ambiguous')
            if (attempt in summaries) != (kind == 'session.compaction.call.usage.1'):
                raise ValueError('Usage does not match an opened provider operation')
            usage = document.get('usage')
            if not isinstance(usage, dict) or usage.get('phase') not in PHASES:
                raise ValueError('Usage observation phase is invalid')
            phase = PHASES[usage['phase']]
            observations.append({'seq': seq, 'kind': kind, 'attempt': attempt, 'phase': usage['phase'],
                                 'payload_sha256': hashlib.sha256(body.encode()).hexdigest(),
                                 'fields': {key: usage.get(key) for key in FIELDS}})
            held = attempts.setdefault(attempt, {})
            if len(attempts) > MAX_ATTEMPTS:
                raise ValueError('Usage attempt bound exceeded')
            for field in FIELDS:
                leaf = usage.get(field)
                if leaf is None:
                    continue
                if (not isinstance(leaf, dict) or type(leaf.get('count')) is not int
                        or not 0 <= leaf['count'] <= MAX_TOKENS
                        or leaf.get('provenance') not in PROVENANCE):
                    raise ValueError('Usage leaf is malformed')
                if (leaf['provenance'] == 'provider-derived' and leaf.get('rule') not in
                        {'fresh-from-inclusive-input', 'inclusive-input-from-partition'}):
                    raise ValueError('Usage derivation rule is missing or invalid')
                held[field] = merge(held.get(field),
                                    (phase, PROVENANCE[leaf['provenance']], leaf['count'],
                                     leaf.get('rule') if leaf['provenance'] == 'provider-derived' else None))
    finally:
        connection.close()
    return dict(attempts=attempts, opened=opened, summaries=summaries, observations=observations,
                opening_seq=opening_seq, bindings=bindings, rows_read=rows_read, bytes_read=bytes_read)


def request_usage(database, session):
    """Per-attempt leaves for request correlation; unknown counts remain absent.

    Uses the same bounded snapshot, binding validation and reconciliation as the
    strict aggregate collector. It does not relax that collector's coverage bar.
    """
    evidence = read_evidence(database, session)
    attempts, opened = evidence['attempts'], evidence['opened']
    summaries, opening_seq, bindings = (evidence[key] for key in ['summaries', 'opening_seq', 'bindings'])
    if not opened or attempts.keys() - opened:
        raise ValueError('Usage has no complete physical-attempt attribution')
    phases = {value: key for key, value in PHASES.items()}
    provenance = {value: key for key, value in PROVENANCE.items()}
    result = []
    for attempt in sorted(opened, key=opening_seq.get):
        fields = {}
        for field in FIELDS:
            leaf = attempts.get(attempt, {}).get(field)
            fields[field] = ({'count': leaf[2], 'phase': phases[leaf[0]],
                              'provenance': provenance[leaf[1]],
                              **({'rule': leaf[3]} if leaf[3] is not None else {})}
                             if leaf is not None and leaf[0] >= 1 and leaf[1] >= 1 else None)
        known = {key: value['count'] for key, value in fields.items() if value is not None}
        if 'inputTotalTokens' in known:
            total = known['inputTotalTokens']
            if any(known.get(key, 0) > total for key in
                   ['inputFreshTokens', 'cacheReadInputTokens', 'cacheWriteInputTokens']):
                raise ValueError('Provider input partition is inconsistent')
            partition = ['inputFreshTokens', 'cacheReadInputTokens', 'cacheWriteInputTokens']
            if (sum(known.get(key, 0) for key in partition) > total or
                    all(key in known for key in partition) and sum(known[key] for key in partition) != total):
                raise ValueError('Provider input partition is inconsistent')
        observed = sum(value is not None for value in fields.values())
        result.append({'attempt': attempt, 'message': bindings.get(attempt),
                       'compaction_summary': attempt in summaries, 'usage': fields,
                       'usage_status': 'observed' if observed == len(FIELDS) else
                                       'partial' if observed else 'unavailable'})
    return result


def snapshot(database, session):
    evidence = read_evidence(database, session)
    attempts, opened, summaries = (evidence[key] for key in ['attempts', 'opened', 'summaries'])
    observations, opening_seq = evidence['observations'], evidence['opening_seq']
    rows_read, bytes_read = evidence['rows_read'], evidence['bytes_read']
    # Tool-only retries can report usage before their terminal binds them.
    # Require complete attribution after all bounded rows have been read.
    if not opened or set(attempts) != opened:
        raise ValueError('Usage does not cover every opened Physical Attempt')
    totals = dict.fromkeys(FIELDS, 0)
    calls = []
    for attempt in sorted(attempts, key=opening_seq.get):
        leaves = attempts[attempt]
        # Absence is unknown; explicit reported zero is a measurement.
        required = ['outputTokens', 'cacheReadInputTokens', 'cacheWriteInputTokens']
        if not all(key in leaves and leaves[key][0] >= 1 and leaves[key][1] >= 1
                   for key in required):
            raise ValueError('Provider usage is incomplete or only estimated')
        values = {key: leaf[2] for key, leaf in leaves.items()
                  if leaf[0] >= 1 and leaf[1] >= 1}
        if 'inputTotalTokens' not in values:
            if 'inputFreshTokens' not in values:
                raise ValueError('Inclusive provider input is unknown')
            values['inputTotalTokens'] = (values['inputFreshTokens']
                                         + values['cacheReadInputTokens']
                                         + values['cacheWriteInputTokens'])
        fresh = values['inputTotalTokens']-values['cacheReadInputTokens']-values['cacheWriteInputTokens']
        if fresh < 0 or values.get('inputFreshTokens', fresh) != fresh:
            raise ValueError('Provider input partition is inconsistent')
        calls.append({'identity': attempt, 'input_tokens': values['inputTotalTokens'],
                      'cached_input_tokens': values['cacheReadInputTokens'],
                      'cache_write_tokens': values['cacheWriteInputTokens'],
                      'compaction_summary': attempt in summaries})
        for key in required+['inputTotalTokens']:
            totals[key] += values[key]
            if totals[key] > MAX_TOKENS:
                raise ValueError('Row usage exceeds the exact token-count bound')
    return {'call_usage': calls, 'usage': {'input_tokens': totals['inputTotalTokens'],
                      'cached_input_tokens': totals['cacheReadInputTokens'],
                      'output_tokens': totals['outputTokens']},
            'cache_write_tokens': totals['cacheWriteInputTokens'],
            'cache_write_tokens_observed': totals['cacheWriteInputTokens'],
            'cache_read_share': (totals['cacheReadInputTokens']/totals['inputTotalTokens']
                                 if totals['inputTotalTokens'] else None),
            'grapt_usage_evidence': {'status': 'complete', 'session': session,
                                      'attempts': len(attempts), 'events_read': rows_read,
                                      'compaction_attempts': len(summaries), 'bytes_read': bytes_read,
                                      'source': ['session.usage.observed.2', 'session.compaction.call.usage.1']},
            'observations': observations}


def collect(database, session, evidence_path=None):
    """Telemetry failure stays explicit without changing a task's measured outcome."""
    try:
        result = snapshot(database, session)
        if evidence_path is not None:
            path = Path(evidence_path)
            if path.exists() or path.is_symlink():
                raise ValueError('Benchmark usage evidence path already exists')
            body = json.dumps(result, indent=2)+'\n'
            path.write_text(body)
            result['grapt_usage_evidence']['artifact'] = path.name
            result['grapt_usage_evidence']['sha256'] = hashlib.sha256(body.encode()).hexdigest()
        result.pop('observations')
        return result
    except (OSError, sqlite3.Error, ValueError, TypeError, AttributeError) as error:
        return {'usage': None, 'cache_write_tokens': None, 'cache_read_share': None,
                'call_usage': None, 'cache_write_tokens_observed': None,
                'grapt_usage_evidence': {'status': 'unavailable', 'reason': str(error)}}
