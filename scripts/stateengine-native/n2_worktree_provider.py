"""Only model replies are scripted; spawn and child effects use production tools."""
import json
import subprocess
import sys

from acceptance_peer import Model
from headless_acceptance import require
from sustained_provider import calls, output, text

CHILD = 'N2_WINDOWS_CHILD: write the isolated checkout and report its contents.'


class WorktreeModel(Model):
    def __init__(self):
        super().__init__()
        self.placement = None
        self.handle = None
        self.polls = 0
        self.result = ''

    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        inputs = body.get('input', [])
        child = any(row.get('role') == 'user' and CHILD in json.dumps(row.get('content'))
                    for row in inputs)
        linked = {row.get('call_id'): row for row in inputs
                  if row.get('type') == 'function_call_output'}
        if not child:
            if 'call_n2_spawn' not in linked:
                return calls([('call_n2_spawn', 'spawn_agent', dict(task_name='windows_writer',
                    message=CHILD, worktree=True, fork_turns='none'))])
            self.placement = output(body, 'call_n2_spawn')
            require(self.placement.get('agent_id') and self.placement.get('directory'),
                    'spawn_agent did not return a child worktree: ' + str(self.placement))
            return text('N2_PARENT_RETURNED_WHILE_CHILD_WORKS')
        previous = 'call_n2_child_exec' if self.polls == 0 else f'call_n2_child_poll_{self.polls}'
        if previous not in linked:
            require(self.handle is None, 'child command was replayed')
            code = ("from pathlib import Path; import time; "
                    "assert Path('parent.txt').read_text()=='committed parent\\n'; "
                    "assert Path('line endings.txt').read_bytes()==b'one\\r\\ntwo\\r\\n'; "
                    "Path('child-written.txt').write_text('N2 child effect\\n'); "
                    "print('N2_CHILD_BEGIN',flush=True); time.sleep(3); "
                    "print('N2_CHILD_DONE',flush=True)")
            return calls([('call_n2_child_exec', 'exec_command', dict(
                cmd=subprocess.list2cmdline([sys.executable, '-u', '-c', code]),
                tty=True, yield_time_ms=250, timeout_ms=20000, max_output_tokens=1000))])
        result = output(body, previous)
        self.result += result.get('output', '')
        if self.handle is None:
            require(result.get('done') is False and result.get('session_id'),
                    'child did not return a real background terminal handle')
            self.handle = result['session_id']
        if not result.get('done'):
            self.polls += 1
            require(self.polls <= 12, 'child background command did not settle')
            return calls([(f'call_n2_child_poll_{self.polls}', 'write_stdin', dict(
                session_id=self.handle, chars='', yield_time_ms=1000, max_output_tokens=1000))])
        require(result.get('exit_code') == 0 and 'N2_CHILD_BEGIN' in self.result
                and 'N2_CHILD_DONE' in self.result, 'child background effect failed: ' + self.result)
        return text('N2_CHILD_VERIFIED')
