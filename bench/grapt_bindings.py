"""Validate native message/retry facts before using them for usage attribution.

Shapes follow grapt-event's session_message and session_tool_terminal codecs.
The caller bounds rows and bytes before decoding; this module performs no I/O.
"""
import re


def object_fields(value, required, optional=()):
    if (not isinstance(value, dict) or not set(required) <= value.keys()
            or value.keys() - set(required) - set(optional)):
        raise ValueError('Native retry binding has missing or unknown fields')
    return value


def text(value):
    if not isinstance(value, str) or not value:
        raise ValueError('Native retry binding identity is invalid')
    return value


def uint(value, maximum=2**64-1):
    if type(value) is not int or not 0 <= value <= maximum:
        raise ValueError('Native retry binding integer is invalid')
    return value


def choice(value, allowed):
    if not isinstance(value, str) or value not in allowed:
        raise ValueError('Native retry binding variant is invalid')
    return value


def message(value):
    if not re.fullmatch(r'msg_[A-Za-z0-9_]+', text(value)):
        raise ValueError('Native retry binding message is invalid')
    return value


def capture(value, length):
    disposition = choice(value.get('disposition'), {'complete', 'truncated'})
    if disposition == 'truncated':
        if uint(value.get('captured')) != length:
            raise ValueError('Native retry binding capture length disagrees')
        return {'captured'}
    return set()


def artifact(value, with_capture=False):
    required = {'id', 'length', 'digest'}
    object_fields(value, required, {'disposition', 'captured'} if with_capture else ())
    text(value['id'])
    text(value['digest'])
    length = uint(value['length'])
    if with_capture:
        object_fields(value, required | {'disposition'} | capture(value, length))
    return length


def opened(value, version):
    required = {'attempt', 'step', 'message'}
    if version == 2:
        required.add('selection')
    object_fields(value, required)
    text(value['attempt'])
    if uint(value['step'], 2**32-1) == 0:
        raise ValueError('Native retry binding step must be positive')
    if version == 2:
        selection = object_fields(value['selection'], {'agent', 'provider', 'model'}, {'variant'})
        for key in selection:
            text(selection[key])
    return message(value['message'])


def block(value):
    object_fields(value, {'attempt', 'message', 'block', 'kind'})
    text(value['attempt'])
    uint(value['block'], 2**32-1)
    choice(value['kind'], {'text', 'reasoning'})
    return message(value['message'])


def appended(value):
    object_fields(value, {'message', 'item'})
    owner = message(value['message'])
    item = object_fields(value['item'], {'ordinal', 'kind', 'payload', 'disposition'},
                         {'call', 'captured'})
    uint(item['ordinal'])
    kind = choice(item['kind'], {'text', 'reasoning', 'tool', 'opaque'})
    captured = capture(item, artifact(item['payload']))
    fields = {'ordinal', 'kind', 'payload', 'disposition'} | captured
    if kind == 'tool':
        fields.add('call')
    object_fields(item, fields)
    return owner, text(item['call']) if kind == 'tool' else None


def terminal(value):
    object_fields(value, {'attempt', 'call', 'outcome'})
    text(value['attempt'])
    call = text(value['call'])
    outcome = object_fields(value['outcome'], {'kind'}, {'output', 'reason', 'detail', 'by'})
    kind = choice(outcome['kind'], {'completed', 'failed', 'cancelled', 'result-missing'})
    required, optional = {'kind'}, set()
    if kind in {'completed', 'failed'}:
        field = 'output' if kind == 'completed' else 'detail'
        optional.add(field)
        if field in outcome:
            artifact(outcome[field], with_capture=True)
    if kind == 'failed':
        required.add('reason')
        choice(outcome.get('reason'), {'authority-unavailable', 'effect-failed',
                                       'capture-failed', 'effect-unknown'})
    if kind == 'cancelled':
        required.add('by')
        choice(outcome.get('by'), {'user', 'managed-shutdown'})
    object_fields(outcome, required, optional)
    return call
