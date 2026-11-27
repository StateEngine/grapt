"""Bounded local MCP peer that asks real nested elicitation/create requests."""
import json
from pathlib import Path
import sys


def send(value):
    data = json.dumps(dict(jsonrpc='2.0', **value))
    if len(data.encode()) > 65536:
        raise ValueError('MCP reply bound')
    print(data, flush=True)


def serve(log):
    pending = {}
    for index in range(128):
        line = sys.stdin.buffer.readline(65537)
        if not line:
            return
        if len(line) > 65536 or not line.endswith(b'\n'):
            raise ValueError('MCP line bound')
        message = json.loads(line)
        method = message.get('method')
        if method == 'initialize':
            send(dict(id=message['id'], result=dict(protocolVersion='2025-06-18',
                capabilities=dict(tools={}), serverInfo=dict(name='elicitor', version='1'))))
        elif method == 'tools/list':
            send(dict(id=message['id'], result=dict(tools=[dict(name='ask', description='Ask a disposable question',
                inputSchema=dict(type='object', properties=dict(case=dict(type='string')), required=['case']))])))
        elif method == 'tools/call':
            case = message['params']['arguments']['case']
            if case not in ('answer', 'decline', 'lost', 'restart', 'defaults', 'formats', 'unsupported-pattern') or len(pending) >= 4:
                raise ValueError('unexpected question')
            with Path(str(log) + '.requests').open('a', encoding='utf-8') as output:
                output.write(case + '\n')
            identity = 'elicitation-' + str(index)
            pending[identity] = (message['id'], case)
            schema = dict(type='object', properties=dict(branch=dict(type='string')),
                          required=['branch'])
            if case == 'answer':
                schema['properties'] = dict(count=dict(type='integer', minimum=1, maximum=3),
                    choice=dict(type='string', enum=['stable', 'preview'], enumNames=['Stable', 'Preview']))
                schema['required'] = ['count', 'choice']
            if case == 'defaults':
                schema = dict(type='object', properties=dict(
                    text=dict(type='string', default='seed'), count=dict(type='integer', default=2, minimum=1, maximum=3),
                    ratio=dict(type='number', default=1.25, minimum=0, maximum=2), flag=dict(type='boolean', default=False),
                    choice=dict(type='string', default='b', enum=['a', 'b'], enumNames=['A', 'B']),
                    many=dict(type='array', default=['a', 'a'], items=dict(type='string', enum=['a', 'b']))),
                    required=['text', 'count', 'ratio', 'flag', 'choice', 'many'])
            if case == 'unsupported-pattern':
                schema = dict(type='object', properties=dict(code=dict(type='string', pattern='^a$')))
            if case == 'formats':
                schema = dict(type='object', properties={key: dict(type='string', default='bad', format=format)
                    for key, format in [('a_email', 'email'), ('b_link', 'uri'), ('c_day', 'date'), ('d_when', 'date-time')]},
                    required=['a_email', 'b_link', 'c_day', 'd_when'])
            send(dict(id=identity, method='elicitation/create' , params=dict(mode='form',
                message='MCP question ' + case, requestedSchema=schema)))
        elif ('result' in message or 'error' in message) and message.get('id') in pending:
            call, case = pending.pop(message['id'])
            with log.open('a', encoding='utf-8') as output:
                output.write(json.dumps(dict(case=case, **{key: message[key] for key in ('result', 'error') if key in message})) + '\n')
            send(dict(id=call, result=dict(content=[dict(type='text', text='MCP_DONE_' + case)])))
        elif method == 'ping':
            send(dict(id=message['id'], result={}))
        elif method and method.startswith('notifications/'):
            continue
        else:
            raise ValueError('unexpected MCP message')
    raise ValueError('MCP request count bound')


if __name__ == '__main__':
    serve(Path(sys.argv[1]))
