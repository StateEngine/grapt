"""Only model responses are scripted; all command, patch and plan effects are real."""
import json
import subprocess
import sys

from acceptance_peer import Model
from sustained_provider import calls, output, text

SOURCE = 'répertoire/計算.py'
INITIAL = b'def add(a, b):\r\n    return abs(a) + abs(b)\r\n'
UPDATED = b'def add(a, b):\r\n    return a + b\r\n'
PATCH = f'''*** Begin Patch
*** Update File: {SOURCE}
@@
-    return abs(a) + abs(b)
+    return a + b
*** Update File: répertoire/ancien.txt
*** Move to: répertoire/déplacé.txt
@@
-before
+after
*** Add File: répertoire/新規.txt
+created
*** Delete File: répertoire/削除.txt
*** End Patch'''


def plan(done=False):
    return dict(explanation='Preserve Windows workflow checklist', plan=[
        dict(step='Reproduce Windows workflow', status='completed'),
        dict(step='Fix Windows workflow', status='completed' if done else 'in_progress'),
        dict(step='Verify Windows workflow', status='completed' if done else 'pending')])


class WorkflowModel(Model):
    def __init__(self):
        super().__init__()
        self.stage = self.polls = self.summaries = 0
        self.handle = self.last = None
        self.outputs = []
        self.collected = ''
        self.interrupted = False
        self.remote_calls = 0
        self.parity_done = False

    def mcp(self, path, _headers, body):
        if path != '/mcp':
            raise ValueError('unexpected remote MCP route')
        method = body.get('method')
        if method == 'initialize':
            result = dict(protocolVersion='2025-06-18', capabilities=dict(tools={}),
                          serverInfo=dict(name='windows-parity', version='1'))
        elif method == 'notifications/initialized':
            return None, b''
        elif method == 'tools/list':
            result = dict(tools=[dict(name='roundtrip', description='Reverse the supplied text.',
                inputSchema=dict(type='object', properties=dict(text=dict(type='string')),
                                 required=['text'], additionalProperties=False))])
        elif method == 'tools/call':
            params = body['params']
            if params['name'] != 'roundtrip' or params['arguments'] != dict(text='remote smoke'):
                raise ValueError('remote MCP call arguments differ')
            self.remote_calls += 1
            result = dict(content=[dict(type='text', text='REMOTE_PARITY: ekoms etomer')], isError=False)
        elif method == 'ping':
            result = {}
        else:
            raise ValueError('unexpected remote MCP method')
        return 'application/json', json.dumps(dict(jsonrpc='2.0', id=body['id'], result=result)).encode()

    def command(self, mode):
        self.last = 'call_windows_' + mode
        self.polls = 0
        self.collected = ''
        self.interrupted = False
        command = subprocess.list2cmdline([sys.executable, 'workflow_command.py', mode, SOURCE])
        return calls([(self.last, 'exec_command', dict(cmd=command, tty=True,
            yield_time_ms=250, timeout_ms=60000, max_output_tokens=1000))])

    def poll(self, body, interrupt=False):
        result = output(body, self.last)
        self.outputs.append(dict(call=self.last, result=result))
        if result.get('omitted_bytes') != 0:
            raise ValueError('command output was omitted')
        self.collected += result.get('output', '')
        if self.polls == 0:
            if result.get('done') is not False or not result.get('session_id'):
                raise ValueError('command did not return a background PTY handle')
            self.handle = result['session_id']
        if not result.get('done'):
            self.polls += 1
            if self.polls > 16:
                raise ValueError('background command exceeded 16 polls')
            self.last = f'call_windows_poll_{self.stage}_{self.polls}'
            send_interrupt = interrupt and not self.interrupted and 'INTERRUPT_READY' in self.collected
            self.interrupted |= send_interrupt
            return calls([(self.last, 'write_stdin', dict(session_id=self.handle,
                chars='\x03' if send_interrupt else '',
                yield_time_ms=1000, max_output_tokens=1000))])
        if interrupt:
            if 'INTERRUPT_READY' not in self.collected or result.get('exit_code') in (None, 0):
                raise ValueError('Ctrl-C did not report a failed/interrupted command')
        elif result.get('exit_code') != 0 or 'BUILD_DONE' not in self.collected:
            raise ValueError('real build/test output or successful exit missing')
        if result.get('cleanup') != 'complete' or result.get('pipe') != 'drained':
            raise ValueError('command exit did not complete cleanup and output draining')
        return None

    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        encoded = json.dumps(body, ensure_ascii=False)
        if 'Output exactly the Markdown structure shown inside <template>' in encoded:
            self.summaries += 1
            if self.summaries != 1 or 'Fix Windows workflow' not in encoded:
                raise ValueError('summary lost the active checklist or ran twice')
            return text('## Objective\n- Finish Windows workflow\n## Work State\n### Completed\n'
                        '- Reproduce Windows workflow; BUILD_DONE; command interrupted\n### Active\n'
                        '- Fix Windows workflow\n### Pending\n- Verify Windows workflow\n'
                        '## Next Move\n1. Verify Windows workflow and complete the checklist.\n')
        if self.stage == 0:
            reply = calls([('call_windows_plan', 'update_plan', plan())])
        elif self.stage == 1:
            reply = calls([('call_windows_patch', 'apply_patch', PATCH)])
        elif self.stage == 2:
            reply = self.command('build')
        elif self.stage == 3:
            pending = self.poll(body)
            if pending:
                return pending
            self.stage = 5
            return self.command('interrupt')
        elif self.stage == 5:
            pending = self.poll(body, interrupt=True)
            if pending:
                return pending
            reply = text('WINDOWS_WORKFLOW_DONE')
        elif self.stage == 6:
            if self.summaries != 1 or 'Fix Windows workflow' not in encoded:
                raise ValueError('continuation lost its active plan after compaction')
            reply = calls([('call_windows_plan_done', 'update_plan', plan(True))])
        elif self.stage == 7:
            reply = text('WINDOWS_PLAN_DONE')
        elif self.stage == 8:
            if 'WINDOWS_ALL_TOOLS' not in encoded or 'AGENTS_PARITY_REQUIRED' not in encoded:
                raise ValueError('tool smoke lost its prompt or project instructions')
            if not any(tool.get('type') == 'web_search' for tool in body.get('tools', [])):
                raise ValueError('native web search was not advertised')
            reply = calls([
                ('call_parity_skill', 'skill', dict(id='parity-check')),
                ('call_parity_image', 'view_image', dict(path='parity.png')),
                ('call_parity_fetch', 'web_fetch', dict(url='https://example.com/')),
                ('call_parity_stdio', 'parity_stdio_journey_echo', dict(text='stdio smoke')),
                ('call_parity_remote', 'parity_remote_roundtrip', dict(text='remote smoke')),
            ])
            # The supplier is scripted here: this sends synthetic hosted-search framing.
            # It does not establish retained replay or an external search.
            search = dict(type='response.output_item.done', item=dict(type='web_search_call',
                id='ws_windows_parity', status='completed', action=dict(type='search',
                    queries=['Windows tool smoke'], sources=[dict(type='url', url='https://example.com/')])))
            reply = reply[0], ('data: ' + json.dumps(search) + '\n\n').encode() + reply[1]
        elif self.stage == 9:
            outputs = {row.get('call_id'): json.dumps(row.get('output'), ensure_ascii=False)
                       for row in body.get('input', [])
                       if row.get('type') in ('function_call_output', 'custom_tool_call_output')}
            for call, marker in [('skill', 'SKILL_PARITY_BODY'), ('image', 'Viewed 1x1'),
                                 ('fetch', 'Example Domain'), ('stdio', 'ZULUQUEBEC7731'),
                                 ('remote', 'REMOTE_PARITY: ekoms etomer')]:
                if marker not in outputs.get('call_parity_' + call, ''):
                    raise ValueError('missing real tool result: ' + call)
            if 'data:image/png;base64,' not in encoded or self.remote_calls != 1:
                raise ValueError('image pixels or exactly one remote MCP call missing')
            self.parity_done = True
            reply = text('WINDOWS_ALL_TOOLS_DONE')
        else:
            raise ValueError('unexpected provider call or replay')
        self.stage += 1
        return reply
