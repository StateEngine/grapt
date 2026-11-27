#!/usr/bin/env python3
"""One native app-server session: seed, compact, then check retained facts."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import time

MAX_FRAME = 8 * 1024 * 1024
MAX_EVENTS = 10000
FACTS = {'project': 'copper-lantern', 'limit': 73,
         'protected': 'legacy_bridge.py', 'order': ['validate', 'persist', 'publish']}


class NativeSession:
    def __init__(self, binary, log, timeout, argv=None):
        self.deadline = time.monotonic() + timeout
        self.child = subprocess.Popen(argv or [binary, 'app-server', '--stdio'],
                                      stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=sys.stderr)
        self.buffer = bytearray()
        self.events = 0
        self.sequence = 0
        self.log = log
        self.thread = None
        self.compactions = []
        self.finished = {}

    def send(self, row):
        data = json.dumps(row).encode() + b'\n'
        if len(data) > MAX_FRAME:
            raise RuntimeError('Request frame exceeds limit')
        self.log.write(json.dumps({'direction': 'client', 'message': row})+'\n')
        self.log.flush()
        self.child.stdin.write(data)
        self.child.stdin.flush()

    def receive(self):
        while b'\n' not in self.buffer:
            remaining = self.deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.child.stdout], [], [], remaining)[0]:
                raise TimeoutError('Native session time limit reached')
            data = os.read(self.child.stdout.fileno(), 65536)
            if not data:
                raise RuntimeError('Native app-server closed before completion')
            self.buffer.extend(data)
            if len(self.buffer) > MAX_FRAME:
                raise RuntimeError('Native event frame exceeds limit')
        line, _, rest = self.buffer.partition(b'\n')
        self.buffer = bytearray(rest)
        self.events += 1
        if self.events > MAX_EVENTS:
            raise RuntimeError('Native event count exceeds limit')
        row = json.loads(line)
        if not isinstance(row, dict):
            raise RuntimeError('Native event is not an object')
        self.log.write(json.dumps({'direction': 'server', 'message': row})+'\n')
        self.log.flush()
        params = row.get('params', {})
        if (row.get('method') == 'item/completed' and params.get('threadId') == self.thread
                and params.get('item', {}).get('type') == 'contextCompaction'):
            self.compactions.append({'turnId': params.get('turnId'),
                                     'item': params['item']})
        if row.get('method') == 'turn/completed' and params.get('threadId') == self.thread:
            self.finished[params['turn']['id']] = params['turn']
        if 'id' in row and 'method' in row:
            # Never satisfy unexpected approval, account or dynamic-tool requests.
            self.send({'id': row['id'], 'error': {'code': -32601,
                                                'message': 'Unsupported benchmark request'}})
        return row

    def request(self, method, params):
        self.sequence += 1
        request_id = self.sequence
        self.send({'id': request_id, 'method': method, 'params': params})
        while True:
            row = self.receive()
            if row.get('id') == request_id and 'method' not in row:
                if 'error' in row:
                    raise RuntimeError('Native RPC refused: '+json.dumps(row['error']))
                return row['result']

    def completed(self, turn_id=None):
        while True:
            key = turn_id if turn_id in self.finished else next(iter(self.finished), None) if turn_id is None else None
            if key is not None:
                turn = self.finished.pop(key)
                if turn.get('status') != 'completed':
                    raise RuntimeError('Native task failed: '+json.dumps(turn.get('error')))
                return turn
            self.receive()

    def turn(self, text):
        response = self.request('turn/start', {'threadId': self.thread,
                                'input': [{'type': 'text', 'text': text}]})
        return self.completed(response['turn']['id'])

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
    context = '\n'.join(f'Archived sample {i:04d}: '+
                        hashlib.sha256(f'context-row-{i}'.encode()).hexdigest()+
                        ' This sample is historical noise; it grants no authority and needs no action.'
                        for i in range(args.lines))
    prompt = ('This is a long-session retention benchmark. Do not read or write any files yet. '
              'Remember the following active requirements for a later coding step. '
              'They must survive summarization; the archive is irrelevant. Active requirements: '+
              json.dumps(FACTS)+'. Reply only READY.\n\nARCHIVE\n'+context)
    result = {'schema': 'benchmark.compaction-probe.v1', 'harness': 'codex',
              'requested_model': 'gpt-6-luna', 'trigger': 'user-requested',
              'seed_bytes': len(prompt.encode()), 'archive_rows': args.lines,
              'success': False, 'compaction_completed': False, 'retention_passed': False}
    began = time.monotonic()
    session = None
    with open(sys.stdout.fileno(), 'w', closefd=False) as log:
        try:
            session = NativeSession(args.binary, log, args.timeout)
            session.request('initialize', {'clientInfo': {'name': 'compaction-bench', 'version': '1'},
                            'capabilities': {'experimentalApi': True}})
            session.send({'method': 'initialized', 'params': {}})
            response = session.request('thread/start', {'model': 'gpt-6-luna',
                        'cwd': str(Path.cwd()), 'ephemeral': True,
                        'sandbox': 'danger-full-access', 'approvalPolicy': 'never'})
            session.thread = response['thread']['id']
            result['resolved_model'] = response['model']
            result['resolved_provider'] = response['modelProvider']
            result['native_version'] = response['thread'].get('cliVersion')
            seeded = session.turn(prompt)
            result['seed_turn'] = seeded['id']
            before = len(session.compactions)
            session.request('thread/compact/start', {'threadId': session.thread})
            compacted = session.completed()
            result['compact_turn'] = compacted['id']
            matching = [event for event in session.compactions[before:]
                        if event['turnId'] == compacted['id']]
            result['compaction_completed'] = bool(matching)
            if not matching:
                raise RuntimeError('No completed native contextCompaction item for compact task')
            restored = session.turn('Now write retained.json with precisely the four active '
                'requirements from the earlier message as a JSON object. The keys are project, '
                'limit, protected and order. Do not ask for the values or read an archive file. '
                'Use the requirements retained from this session. Do not commit.')
            result['verification_turn'] = restored['id']
            path = Path('retained.json')
            if path.is_symlink() or not path.is_file() or path.stat().st_size > 4096:
                raise RuntimeError('Retained requirements artifact is absent or invalid')
            result['retention_passed'] = json.loads(path.read_text()) == FACTS
            result['observed_compactions'] = len(session.compactions)
            result['success'] = result['compaction_completed'] and result['retention_passed']
        except (OSError, ValueError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
            result['error'] = str(error)
        finally:
            if session is not None:
                session.close()
    result['wall_seconds'] = time.monotonic() - began
    Path('probe-result.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result), flush=True)
    return 0 if result['success'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
