"""Join bounded local request comparisons to durable per-attempt provider usage.

A local request change is an observation, never the provider's cache-miss cause.
No request body, credential, affinity token or prompt hash belongs in this report.
"""
import json
from pathlib import Path
import re
import sqlite3

import provider_timing
import grapt_usage

MAX_LINE = 8192
CHANGES = {'first_observation', 'target_changed', 'cache_key_changed', 'routing_changed',
           'tools_changed', 'instructions_changed', 'options_changed', 'input_changed',
           'input_shortened', 'append_only', 'identical', 'comparison_bound', 'unreported'}


def identity(value):
    if not isinstance(value, str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,128}', value):
        raise ValueError('Invalid request comparison identity')
    return value


def integer(value, maximum):
    if value is None:
        return None
    if type(value) is not int or not 0 <= value <= maximum:
        raise ValueError('Invalid request comparison count')
    return value


def metadata(value):
    if value is None:
        return None
    if not isinstance(value, dict):
        raise ValueError('Invalid request comparison metadata')
    change = value.get('cacheRequestChange')
    if not isinstance(change, str) or change not in CHANGES:
        raise ValueError('Unknown request comparison label')
    index = value.get('cacheRequestChangedItem')
    if index is not None:
        if not isinstance(index, str) or not re.fullmatch(r'\d{1,5}', index):
            raise ValueError('Invalid changed input index')
        index = integer(int(index), 4096)
    if (change in {'input_changed', 'input_shortened'}) != (index is not None):
        raise ValueError('Request comparison label and input index disagree')
    previous = value.get('cacheRequestPreviousAttempt')
    return {'change': change, 'changed_item': index,
            'previous_attempt': identity(previous) if previous is not None else None,
            **{target: integer(value.get(source), 16*1024*1024) for source, target in [
                ('cacheRequestBodyBytes', 'body_bytes'), ('cacheRequestToolsBytes', 'tools_bytes'),
                ('cacheRequestInstructionsBytes', 'instructions_bytes')]}}


def loss_record(line):
    parts = line.split()
    if not parts or parts[0] not in {'GRAPT_PROVIDER_DISPATCH_LOSS', 'GRAPT_PROVIDER_TAIL_LOSS'}:
        return None
    allowed = ({'count_at_least': 'dispatch'} if parts[0] == 'GRAPT_PROVIDER_DISPATCH_LOSS'
               else {'tail_count_at_least': 'output', 'acp_count_at_least': 'acp'})
    result = {}
    if len(parts) == 1:
        raise ValueError('Request comparison loss record has no count')
    for field in parts[1:]:
        name, separator, value = field.partition('=')
        if (name not in allowed or allowed[name] in result or separator != '='
                or not re.fullmatch(r'[0-9]{1,20}', value)):
            raise ValueError('Invalid request comparison loss field')
        result[allowed[name]] = integer(int(value), 2**64-1)
    return result


def samples(log, session):
    result, loss = {}, {'dispatch': None, 'output': None, 'acp': None}
    for line in provider_timing.diagnostic_lines(log):
        observed_loss = loss_record(line)
        if observed_loss is not None:
            for kind, count in observed_loss.items():
                loss[kind] = max(loss[kind] or 0, count)
            continue
        match = provider_timing.PREFIX.match(line)
        if match is None or ' cache_request=' not in line:
            continue
        if len(line.encode()) > MAX_LINE:
            raise ValueError('Request comparison line exceeds its bound')
        document = json.loads(line.split(' cache_request=', 1)[1])
        if not isinstance(document, dict):
            raise ValueError('Invalid request comparison record')
        if document.get('session') != session:
            continue
        attempt = identity(match[1])
        if attempt in result or len(result) == grapt_usage.MAX_ATTEMPTS:
            raise ValueError('Duplicate or excessive request comparison samples')
        result[attempt] = metadata(document.get('metadata'))
    return result, loss


def collect(log, database, session, evidence_path=None):
    """Capture while the private store exists, retaining missing sides of the join."""
    result = {'status': 'unavailable', 'session': session, 'calls': None,
              'provider_miss_cause': 'unreported',
              'comparison_scope': 'previous observed request in the same process and Session',
              'limits': ['Local request changes do not establish a provider cache-miss cause.',
                         'Missing, estimated or stream-only usage remains unavailable.',
                         'Process restart, bounded retention and diagnostic loss can remove comparisons.']}
    calls, captured = [], {}
    loss = {'dispatch': None, 'output': None, 'acp': None}
    try:
        calls = grapt_usage.request_usage(database, session)
    except (OSError, sqlite3.Error, ValueError, TypeError, AttributeError):
        # Error strings may contain database payloads or paths. Do not export them.
        result['usage_capture_error'] = 'Physical-attempt usage attribution is absent, malformed or outside bounds'
    try:
        if not isinstance(session, str) or not re.fullmatch(r'ses_[A-Za-z0-9_]{1,124}', session):
            raise ValueError('Invalid request comparison Session')
        captured, loss = samples(log, session)
    except (OSError, ValueError, TypeError, UnicodeError):
        result['request_capture_error'] = 'Request comparisons are absent, malformed or outside bounds'
    owned = {call['attempt'] for call in calls}
    for attempt in captured:
        if attempt in owned:
            continue
        calls.append({'attempt': attempt, 'message': None, 'compaction_summary': None,
                      'usage': dict.fromkeys(grapt_usage.FIELDS), 'usage_status': 'unavailable'})
    for call in calls:
        request = captured.get(call['attempt'])
        call.update(request=request, provider_miss_cause='unreported',
                    attribution='durable' if call['attempt'] in owned else 'dispatch-only',
                    request_status='unavailable' if request is None or request['change'] == 'unreported'
                                   else 'limited' if request['change'] == 'comparison_bound' else 'observed')
    complete = bool(calls) and all(call['request_status'] == 'observed'
                                  and call['usage_status'] == 'observed'
                                  and call['attribution'] == 'durable' for call in calls)
    result.update(status='complete' if complete and not any(loss.values()) else
                         'partial' if calls else 'unavailable',
                  calls=calls or None, diagnostic_loss_at_least=loss,
                  unattributed_request_samples=len(captured.keys()-owned))
    if evidence_path is not None:
        try:
            path = Path(evidence_path)
            if path.exists() or path.is_symlink():
                raise ValueError('Existing request comparison evidence')
            with path.open('x') as output:
                output.write(json.dumps(result, indent=2)+'\n')
            result['artifact'] = path.name
        except (OSError, ValueError):
            result['artifact_error'] = 'Request comparison evidence could not be written to a new file'
    return {'request_cache': result}
