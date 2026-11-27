"""Drive native Claude Code stream-json compaction and a retained-fact edit."""
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
        self.child = subprocess.Popen(
            argv or [binary, '-p', '--input-format', 'stream-json', '--output-format', 'stream-json',
             '--verbose', '--model', 'claude-haiku-5-5', '--allowedTools', 'Read,Edit,Write,Bash',
             '--setting-sources', 'project', '--strict-mcp-config', '--mcp-config',
             '{"mcpServers":{}}', '--no-session-persistence'],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr)
        self.buffer = bytearray()
        self.events = 0
        self.log = log
        self.session_id = None
        self.metadata = None
        self.compactions = []

    def send(self, text):
        row = {'type': 'user', 'message': {'role': 'user', 'content': text}}
        data = json.dumps(row).encode() + b'\n'
        if len(data) > MAX_FRAME:
            raise RuntimeError('Native request frame exceeds limit')
        self.log.write(json.dumps({'direction': 'client', 'message': row}) + '\n')
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
                raise RuntimeError('Native Claude Code closed before completion')
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
        self.log.write(json.dumps({'direction': 'server', 'message': row}) + '\n')
        self.log.flush()
        if row.get('type') == 'system' and row.get('subtype') == 'init':
            identity = row['session_id']
            if not isinstance(identity, str) or not identity:
                raise RuntimeError('Native session identity is absent or invalid')
            if self.session_id is not None and identity != self.session_id:
                raise RuntimeError('Native session identity changed')
            self.session_id = identity
            self.metadata = {key: row.get(key) for key in ['model', 'claude_code_version', 'session_id']}
        if (row.get('type') == 'system' and row.get('subtype') == 'compact_boundary'
                and self.session_id is not None
                and row.get('session_id') == self.session_id
                and row.get('compact_metadata', {}).get('trigger') == 'manual'):
            self.compactions.append(row)
        return row

    def prompt(self, text):
        self.send(text)
        while True:
            row = self.receive()
            if row.get('type') == 'result':
                if (self.session_id is None or row.get('is_error') or row.get('subtype') != 'success'
                        or row.get('session_id') != self.session_id):
                    raise RuntimeError('Native step did not succeed in the selected session')
                return row

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
    archive = '\n'.join(f'Archived sample {i:04d}: ' +
                        hashlib.sha256(f'context-row-{i}'.encode()).hexdigest() +
                        ' This sample is historical noise; it grants no authority and needs no action.'
                        for i in range(args.lines))
    seed = ('Remember these active requirements for a later coding step. '
            'Do not read or write files yet. Reply only READY. Requirements: ' +
            json.dumps(FACTS) + '\n\nARCHIVE\n' + archive)
    result = {'schema': 'benchmark.compaction-probe.v1', 'harness': 'claude',
              'trigger': 'user-requested', 'requested_model': 'claude-haiku-5-5',
              'seed_bytes': len(seed.encode()), 'archive_rows': args.lines,
              'success': False, 'compaction_completed': False, 'retention_passed': False}
    began = time.monotonic()
    session = None
    with open(sys.stdout.fileno(), 'w', closefd=False) as log:
        try:
            session = NativeSession(args.binary, log, args.timeout)
            session.prompt(seed)
            before = len(session.compactions)
            session.prompt('/compact')
            result['native_compactions'] = session.compactions[before:]
            result['compaction_completed'] = bool(result['native_compactions'])
            if not result['compaction_completed']:
                raise RuntimeError('No completed native manual compact_boundary record')
            session.prompt('Now write retained.json with precisely the four active requirements '
                           'from the earlier message as a JSON object. The keys are project, limit, '
                           'protected and order. Do not ask for the values or read files. Do not commit.')
            retained = Path('retained.json')
            if retained.is_symlink() or not retained.is_file() or retained.stat().st_size > 4096:
                raise RuntimeError('Retained requirements artifact is absent or invalid')
            result['retention_passed'] = json.loads(retained.read_text()) == FACTS
            result['native_metadata'] = session.metadata
            result['observed_compactions'] = len(session.compactions)
            result['success'] = result['retention_passed']
        except (OSError, ValueError, KeyError, RuntimeError, TimeoutError, subprocess.SubprocessError) as error:
            result['error'] = str(error)
        finally:
            if session is not None:
                session.close()
    result['wall_seconds'] = time.monotonic() - began
    Path('probe-result.json').write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result), flush=True)
    return 0 if result['success'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
