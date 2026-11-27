"""Identify copied Physical Attempts at the durable history-fork boundary."""
import hashlib
import json
import re

import grapt_bindings


def fingerprint(document):
    return hashlib.sha256(json.dumps(document, sort_keys=True, separators=(',', ':')).encode()).digest()


def inherited_attempts(connection, session, decode, max_attempts, max_rows, max_row_bytes):
    rows = connection.execute(
        "SELECT type, CASE WHEN length(CAST(data AS BLOB)) <= ? THEN data END "
        "FROM event WHERE aggregate_family='session' AND aggregate_id=? "
        "AND type LIKE 'session.forked.%' ORDER BY seq LIMIT 2", (max_row_bytes, session))
    forks = [(kind, decode(body)) for kind, body in rows]
    if not forks:
        return set(), {}
    if len(forks) != 1 or forks[0][0] != 'session.forked.2':
        raise ValueError('Usage fork boundary is repeated or unsupported')
    fork = forks[0][1]
    parent, through = fork.get('parentID'), fork.get('parentSeq')
    if (not {'sessionID', 'parentID', 'parentSeq'} <= fork.keys()
            or fork.keys() - {'sessionID', 'parentID', 'parentSeq', 'from'}
            or fork['sessionID'] != session or parent == session
            or not isinstance(parent, str) or not re.fullmatch(r'ses_[A-Za-z0-9_]{1,124}', parent)
            or type(through) is not int or not -1 <= through < 2**63):
        raise ValueError('Usage fork boundary is malformed')
    if 'from' in fork:
        grapt_bindings.message(fork['from'])
    if through == -1:
        return set(), {}
    exists = connection.execute(
        "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family='session' "
        "AND aggregate_id=? AND seq=?)", (parent, through)).fetchone()[0]
    if not exists:
        raise ValueError('Usage fork boundary has no recorded parent event')
    # The lifecycle owner copies Physical Attempt IDs and usage unchanged,
    # while reminting message IDs. Only the frozen parent prefix proves which
    # attempts the child inherited; later parent calls cannot be subtracted.
    rows = connection.execute(
        "SELECT type, CASE WHEN length(CAST(data AS BLOB)) <= ? THEN data END "
        "FROM event WHERE aggregate_family='session' AND aggregate_id=? AND seq<=? "
        "AND (type LIKE 'session.assistant-message.opened.%' "
        "OR type LIKE 'session.usage.observed.%' "
        "OR type IN ('session.created.1', 'session.content-block.opened.1', "
        "'session.tool.terminal.1')) ORDER BY seq LIMIT ?",
        (max_row_bytes, parent, through, max_rows + 1))
    attempts = set()
    usage = {}
    found = False
    for kind, body in rows:
        document = decode(body)
        found = True
        if kind == 'session.created.1':
            continue
        if kind not in {'session.assistant-message.opened.1', 'session.assistant-message.opened.2',
                        'session.usage.observed.1', 'session.usage.observed.2',
                        'session.content-block.opened.1', 'session.tool.terminal.1'}:
            raise ValueError('Inherited usage version is unsupported')
        attempt = document.get('attempt')
        if not isinstance(attempt, str) or not 1 <= len(attempt) <= 128:
            raise ValueError('Inherited usage attempt identity is missing or invalid')
        attempts.add(attempt)
        if len(attempts) > max_attempts:
            raise ValueError('Inherited usage attempt bound exceeded')
        if kind.startswith('session.usage.observed.'):
            if document.get('sessionID') == parent:
                document = {**document, 'sessionID': session}
            usage.setdefault((kind, attempt), set()).add(fingerprint(document))
    if not found:
        raise ValueError('Usage fork parent evidence is unavailable')
    return attempts, usage
