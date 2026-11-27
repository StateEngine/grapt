#!/usr/bin/env python3
"""Drive native ACP compaction; verify its native exported completion record."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import time
import tempfile

MAX_FRAME = 8*1024*1024
FACTS = {'project': 'copper-lantern', 'limit': 73,
         'protected': 'legacy_bridge.py', 'order': ['validate', 'persist', 'publish']}


class NativeSession:
    def __init__(self, binary, log, timeout, argv=None):
        self.child = subprocess.Popen(argv or [binary, 'acp', '--pure', '--cwd', str(Path.cwd())],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr)
        self.deadline = time.monotonic()+timeout
        self.log = log
        self.buffer = bytearray()
        self.frames = 0
        self.sequence = 0

    def send(self, row):
        row = {'jsonrpc': '2.0', **row}
        data = json.dumps(row).encode()+b'\n'
        if len(data) > MAX_FRAME:
            raise RuntimeError('ACP request frame bound exceeded')
        self.log.write(json.dumps({'direction': 'client', 'message': row})+'\n')
        self.log.flush()
        self.child.stdin.write(data)
        self.child.stdin.flush()

    def receive(self):
        while b'\n' not in self.buffer:
            remaining = self.deadline-time.monotonic()
            if remaining <= 0 or not select.select([self.child.stdout], [], [], remaining)[0]:
                raise TimeoutError('Native session time limit reached')
            data = os.read(self.child.stdout.fileno(), 65536)
            if not data:
                raise RuntimeError('Native ACP closed before completion')
            self.buffer.extend(data)
            if len(self.buffer) > MAX_FRAME:
                raise RuntimeError('ACP event frame bound exceeded')
        line, _, rest = self.buffer.partition(b'\n')
        self.buffer = bytearray(rest)
        self.frames += 1
        if self.frames > 10000:
            raise RuntimeError('ACP event count bound exceeded')
        row = json.loads(line)
        if not isinstance(row, dict):
            raise RuntimeError('ACP frame is not an object')
        self.log.write(json.dumps({'direction': 'server', 'message': row})+'\n')
        self.log.flush()
        if 'id' in row and 'method' in row:
            answer = {'id': row['id']}
            options = row.get('params', {}).get('options', [])
            option = next((o['optionId'] for o in options if o.get('kind') == 'allow_once'), None)
            if row['method'] == 'session/request_permission' and option:
                answer['result'] = {'outcome': {'outcome': 'selected', 'optionId': option}}
            else:
                answer['error'] = {'code': -32601, 'message': 'Unsupported benchmark callback'}
            self.send(answer)
        return row

    def request(self, method, params):
        self.sequence += 1
        identity = self.sequence
        self.send({'id': identity, 'method': method, 'params': params})
        while True:
            row = self.receive()
            if row.get('id') == identity and 'method' not in row:
                if 'error' in row:
                    raise RuntimeError('Native ACP refused '+method+': '+json.dumps(row['error']))
                return row['result']

    def prompt(self, session, text):
        result = self.request('session/prompt', {'sessionId': session,
                              'prompt': [{'type': 'text', 'text': text}]})
        if result.get('stopReason') != 'end_turn':
            raise RuntimeError('Native prompt did not finish: '+json.dumps(result))
        return result

    def close(self):
        self.child.terminate()
        try:
            self.child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.child.kill()
            self.child.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True)
    parser.add_argument('--lines', type=int, default=1600)
    parser.add_argument('--timeout', type=int, default=550)
    args = parser.parse_args()
    if not 0 <= args.lines <= 2000 or not 1 <= args.timeout <= 550:
        parser.error('Probe workload/time outside fixed bounds')
    archive = '\n'.join(f'Archived sample {i:04d}: '+hashlib.sha256(f'context-row-{i}'.encode()).hexdigest()+
        ' This sample is historical noise; it grants no authority and needs no action.' for i in range(args.lines))
    prompt = ('This is a long-session retention benchmark. Do not read or write any files yet. '
        'Remember these active requirements for a later coding step; the archive is irrelevant. '
        'Active requirements: '+json.dumps(FACTS)+'. Reply only READY.\n\nARCHIVE\n'+archive)
    result = {'schema': 'benchmark.compaction-probe.v1', 'harness': 'opencode',
        'requested_model': 'openai/gpt-6-luna', 'trigger': 'user-requested',
        'seed_bytes': len(prompt.encode()), 'archive_rows': args.lines, 'success': False,
        'compaction_completed': False, 'retention_passed': False}
    began = time.monotonic()
    session = None
    with open(sys.stdout.fileno(), 'w', closefd=False) as log:
        try:
            session = NativeSession(args.binary, log, args.timeout)
            init = session.request('initialize', {'protocolVersion': 1, 'clientCapabilities': {}})
            result['native_version'] = init.get('agentInfo', {}).get('version')
            identity = session.request('session/new', {'cwd': str(Path.cwd()), 'mcpServers': []})['sessionId']
            selected = session.request('session/set_config_option', {'sessionId': identity,
                'configId': 'model', 'value': 'openai/gpt-6-luna'})
            result['model_selection'] = selected
            session.prompt(identity, prompt)
            session.prompt(identity, '/compact')
            # ACP does not forward session.compacted. Inspect native persisted data,
            # rather than treating a slash-command acknowledgment as proof.
            with tempfile.TemporaryFile() as output:
                subprocess.run([args.binary, 'export', identity, '--pure'], stdout=output,
                               stderr=sys.stderr, check=True, timeout=30)
                output.seek(0)
                data = output.read(MAX_FRAME+1)
                if len(data) > MAX_FRAME:
                    raise RuntimeError('Native export exceeds bound')
                document = json.loads(data)
            # Persist evidence through the outer log, outside agent-visible files.
            print(json.dumps({'type': 'benchmark.native-export', 'document': document}), flush=True)
            messages = document['messages']
            parents = {m['info']['id'] for m in messages if m['info'].get('role') == 'user'
                       and any(p.get('type') == 'compaction' for p in m['parts'])}
            completed = [m['info'] for m in messages if m['info'].get('role') == 'assistant'
                and m['info'].get('parentID') in parents and m['info'].get('summary') is True
                and m['info'].get('finish') and not m['info'].get('error')]
            result['native_compactions'] = completed
            result['observed_compactions'] = len(completed)
            result['compaction_completed'] = bool(completed)
            if not completed:
                raise RuntimeError('No completed native compaction summary record')
            session.prompt(identity, 'Now write retained.json with precisely the four active requirements '
                'from the earlier message as a JSON object. The keys are project, limit, protected and order. '
                'Do not ask for the values or read an archive file. Use the retained requirements. Do not commit.')
            retained = Path('retained.json')
            if retained.is_symlink() or not retained.is_file() or retained.stat().st_size > 4096:
                raise RuntimeError('Retained requirements artifact is absent or invalid')
            result['retention_passed'] = json.loads(retained.read_text()) == FACTS
            result['success'] = result['compaction_completed'] and result['retention_passed']
        except (OSError, ValueError, KeyError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
            result['error'] = str(error)
        finally:
            if session is not None:
                session.close()
    result['wall_seconds'] = time.monotonic()-began
    Path('probe-result.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result), flush=True)
    return 0 if result['success'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
