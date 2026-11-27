"""Controlled stdio peer for child-generation and no-replay regression tests."""
import json
from pathlib import Path
import os
import sys

root = Path(sys.argv[1])
generation = (root / 'generation').read_text().strip()
support_path = root / 'key-support'
support = support_path.read_text().strip() if support_path.exists() else ''
key_support = support in ('all', generation)
with (root / 'starts').open('a') as log:
    log.write(str(os.getpid()) + '\n')
if (root / 'fail-start').exists():
    raise SystemExit('requested fixture startup refusal')
for _ in range(64):
    raw = sys.stdin.readline(1048577)
    if not raw:
        break
    if len(raw) > 1048576:
        raise SystemExit(2)
    request = json.loads(raw)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        value = dict(protocolVersion=request['params']['protocolVersion'], capabilities={'tools': {}},
                     serverInfo=dict(name='producer-version-fixture', version=generation))
    elif method == 'tools/list':
        value = dict(tools=[dict(name=name, inputSchema={
            'type': 'object', 'properties': {'idempotency_key': {'type': 'string'}}
            if key_support and name == 'stateengine_msg_send' else {}}) for name in
            ['reader', 'stateengine_msg_read', 'stateengine_msg_list', 'stateengine_resume',
             'stateengine_msg_send']])
    elif method == 'tools/call':
        arguments = request['params']['arguments']
        action = arguments.get('action', 'read')
        key = arguments.get('idempotency_key') if key_support and request['params']['name'] == 'stateengine_msg_send' else None
        receipts_path = root / 'receipts'
        receipts = json.loads(receipts_path.read_text()) if receipts_path.exists() else {}
        previous = receipts.get(key) if isinstance(key, str) else None
        conflict = previous is not None and previous['arguments'] != arguments
        if isinstance(key, str) and previous is None:
            receipts[key] = dict(arguments=arguments, msg_id='durable-message-1')
            receipts_path.write_text(json.dumps(receipts))
        binding = request['params'].get('_meta', {}).get('stateengine.session-capability-v1')
        if binding not in (None, 'private-fixture-binding'):
            raise SystemExit('fixture binding changed')
        if action == 'effect-then-error' and previous is None:
            with (root / 'effects').open('a') as effects:
                effects.write('effect\n')
        with (root / 'calls').open('a') as log:
            log.write(json.dumps(dict(pid=os.getpid(), action=action, name=request['params']['name'], generation=generation,
                                      binding_present=binding is not None, arguments=arguments)) + '\n')
        stale = generation != (root / 'generation').read_text().strip()
        failure = stale or (action == 'effect-then-error' and key is None) or (root / 'always-refuse').exists()
        text = ('content conflict: durable-message-1' if conflict else
                'Error: mixed-engine-versions: producer changed' if failure else
                'durable-message-1' if key is not None else 'current producer')
        value = dict(content=[dict(type='text', text=text)], isError=conflict or failure)
    else:
        raise SystemExit('unexpected fixture request')
    print(json.dumps(dict(jsonrpc='2.0', id=request['id'], result=value)), flush=True)
else:
    raise SystemExit('fixture request count bound')
