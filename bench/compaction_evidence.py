"""Count completed native automatic compactions, never command acknowledgments."""


def _identity(value):
    if not isinstance(value, str) or not value or len(value) > 256:
        raise ValueError('Native compaction identity is absent or oversized')
    return value


def opencode(document, session):
    if document.get('info', {}).get('id') != session:
        raise ValueError('OpenCode export names another Session')
    messages = document.get('messages')
    if not isinstance(messages, list) or len(messages) > 4096:
        raise ValueError('OpenCode export message census is invalid')
    parents = {_identity(row['info']['id']) for row in messages
               if row.get('info', {}).get('role') == 'user'
               and any(part.get('type') == 'compaction' and part.get('auto') is True
                       for part in row.get('parts', []))}
    completed = {}
    for row in messages:
        info = row.get('info', {})
        if (info.get('role') == 'assistant' and info.get('parentID') in parents
                and info.get('summary') is True and info.get('finish')
                and not info.get('error')):
            identity = _identity(info.get('id'))
            completed[identity] = {'id': identity, 'trigger': 'auto',
                                   'parent': info['parentID']}
    return list(completed.values())


def grapt(pages, session):
    completed = {}
    if not pages or len(pages) > 32:
        raise ValueError('SE public history census is absent or oversized')
    for index, page in enumerate(pages):
        if page.get('session') != session:
            raise ValueError('SE public history names another Session')
        body = page.get('body', {})
        if (not isinstance(body.get('data'), list) or len(body['data']) > 512
                or not isinstance(body.get('cursor'), dict)):
            raise ValueError('SE public history page is malformed')
        next_cursor = body.get('cursor', {}).get('next')
        if bool(next_cursor) != (index < len(pages)-1):
            raise ValueError('SE public history census is incomplete')
        for row in body['data']:
            if (row.get('type') == 'compaction' and row.get('status') == 'completed'
                    and row.get('reason') == 'auto'):
                metadata = row.get('metadata', {})
                identity = _identity(metadata.get('compactionID'))
                window = _identity(metadata.get('contextWindowID'))
                if (type(metadata.get('epoch')) is not int or metadata['epoch'] < 0
                        or metadata.get('trigger') not in {'policy-threshold', 'structural-bound',
                                                          'context-overflow', 'emergency-headroom'}):
                    raise ValueError('SE automatic compaction metadata is invalid')
                value = {'id': identity, 'trigger': metadata['trigger'],
                         'window': window, 'epoch': metadata['epoch']}
                if identity in completed and completed[identity] != value:
                    raise ValueError('SE compaction identity has conflicting metadata')
                completed[identity] = value
    return list(completed.values())


def codex(events, thread):
    completed = {}
    for event in events:
        params = event.get('params', {})
        item = params.get('item', {})
        if (event.get('method') == 'item/completed' and params.get('threadId') == thread
                and item.get('type') == 'contextCompaction'):
            identity = _identity(item.get('id'))
            completed[identity] = {'id': identity, 'trigger': 'automatic-no-manual-request',
                                   'step': _identity(params.get('turnId'))}
    return list(completed.values())


def claude(events, session):
    completed = {}
    for ordinal, event in enumerate(events):
        if (event.get('type') == 'system' and event.get('subtype') == 'compact_boundary'
                and event.get('session_id') == session
                and event.get('compact_metadata', {}).get('trigger') == 'auto'):
            # Stream order supplies identity when the native record has no UUID.
            identity = event.get('uuid', f'boundary-{ordinal}')
            completed[_identity(identity)] = {'id': identity, 'trigger': 'auto',
                                             'metadata': event['compact_metadata']}
    return list(completed.values())


def coverage(completed, continued):
    if completed is None:
        return {'observed_compactions': None, 'compaction_coverage': 'unknown'}
    return {'observed_compactions': len(completed),
            'compaction_coverage': 'passed' if completed and continued else 'failed',
            'automatic_compactions': completed, 'continued_same_session': continued}
