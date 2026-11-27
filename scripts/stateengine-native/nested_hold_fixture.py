"""Bounded local MCP effect whose missing result must never cause automatic replay."""
import json
from pathlib import Path
import sys
import time
from elicitation_fixture import send

root = Path(sys.argv[1])
for index in range(64):
    line = sys.stdin.buffer.readline(65537)
    if not line: break
    if len(line)>65536 or not line.endswith(b'\n'): raise ValueError('MCP request byte bound')
    message = json.loads(line)
    method = message.get('method')
    if method == 'initialize':
        result = dict(protocolVersion='2025-06-18', capabilities=dict(tools={}), serverInfo=dict(name='held',version='1'))
    elif method == 'tools/list':
        result = dict(tools=[dict(name='hold', description='Record a disposable task effect then wait.',
            inputSchema=dict(type='object',properties={}))])
    elif method == 'tools/call':
        if message['params']['name'] != 'hold': raise ValueError('unexpected tool')
        with (root/'effect.txt').open('a') as effect: effect.write('effect recorded once\n')
        (root/'entered').write_text('ready')
        for _ in range(1200):
            if (root/'release').exists(): break
            time.sleep(.1)
        result = dict(content=[dict(type='text',text='completed')])
    elif method and method.startswith('notifications/'): continue
    else: raise ValueError('unexpected MCP message')
    send(dict(id=message['id'],result=result))
else:
    raise ValueError('MCP request count bound')
