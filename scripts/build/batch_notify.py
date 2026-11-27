"""Persist phase notification debt independently of canonical publication."""
import re
import hashlib
import json
import subprocess

import nightly_revert


def configure(actor, mappings, numbers):
    if not actor or not re.fullmatch(r'agt-[a-z0-9-]{3,80}', actor):
        raise ValueError('Configure --actor with the registered notification identity')
    authors = {}
    for value in mappings:
        number, separator, agent = value.partition('=')
        if not separator or not number.isdecimal() or not re.fullmatch(r'agt-[a-z0-9-]{3,80}', agent):
            raise ValueError('Use --author PR=REGISTERED_AGENT for every included author')
        number = int(number)
        if number in authors:
            raise ValueError('Duplicate pull request author mapping')
        authors[number] = agent
    if set(authors) != set(numbers):
        raise ValueError('Explicit author mapping must cover every requested PR exactly')
    agents, lead = nightly_revert.registry()
    known = {row['agent_id'] for row in agents}
    if not {actor, lead, *authors.values()} <= known:
        raise ValueError('Notification identity or author is not registered')
    return dict(actor=actor, lead=lead, authors={str(n): a for n, a in authors.items()})


def owners(names):
    agents, _lead = nightly_revert.registry()
    result = set()
    for name in names:
        matches = {row['agent_id'] for row in agents if row.get('display_name') == name or row['agent_id'] == name}
        if len(matches) != 1:
            raise ValueError('Quarantine owner does not identify one registered agent')
        result.update(matches)
    return sorted(result)


def enqueue(state, key, phase, result, numbers, extra=()):
    config = state['mail']
    outbox = state.setdefault('notifications', [])
    if any(row['key'] == key for row in outbox):
        return
    if len(outbox) >= 128:
        raise ValueError('Notification history is full; preserve the controller state')
    targets = sorted({config['lead'], *(config['authors'][str(n)] for n in numbers), *extra})
    body = f'Batch {state["name"]}: {phase}: {result}'
    if len(body) > 4096:
        raise ValueError('Batch phase result exceeds its notification bound')
    outbox.append(dict(key=key, body=body, targets=targets, delivered=[]))


def flush(state, persist):
    for row in state.get('notifications', []):
        for recipient in row['targets']:
            if recipient in row['delivered']:
                continue
            try:
                identity = json.dumps([state['mail']['actor'], state['name'], row['key'],
                                       recipient, row['body']], ensure_ascii=True, separators=(',', ':'))
                key = 'batch-land:' + hashlib.sha256(identity.encode('utf-8')).hexdigest()
                nightly_revert.send(state['mail']['actor'], recipient, row['body'], timeout=30,
                                    idempotency_key=key)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                row['error'] = str(error)[-1024:]
                persist()
                # An unavailable mail door must not hold every phase on every recipient.
                return False
            row['delivered'].append(recipient)
            row.pop('error', None)
            persist()
    return True
