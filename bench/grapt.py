"""Headless ACP client: the native product retains model, tools and permissions."""
import argparse
import json
import re
from pathlib import Path
import subprocess
import sys

MAX_FRAME = 8*1024*1024
MAX_ERROR_MESSAGE = 512


def request_error(method, error):
    message = error.get('message') if isinstance(error, dict) else None
    if not isinstance(message, str) or not message.strip():
        return 'Native ACP request refused: '+method
    message = re.sub(r'sk-ant-[A-Za-z0-9_-]*', '[REDACTED]', message)
    message = ''.join(character if character.isprintable() else ' ' for character in message)
    return 'Native ACP request failed: '+method+': '+message[:MAX_ERROR_MESSAGE]


def permission(params):
    # Same unattended policy as the other adapters, inside a disposable sandbox.
    options = params.get('options', [])
    option = next((row.get('optionId') for row in options if row.get('kind') == 'allow_once'), None)
    return {'outcome': {'outcome': 'selected', 'optionId': option}} if option else {'outcome': {'outcome': 'cancelled'}}


class Client:
    def __init__(self, child):
        self.child = child
        self.ordinal = 0
        self.frames = 0

    def send(self, row):
        body = json.dumps(row).encode()+b'\n'
        if len(body) > MAX_FRAME: raise RuntimeError('ACP outgoing frame bound exceeded')
        self.child.stdin.write(body); self.child.stdin.flush()

    def request(self, method, params):
        self.ordinal += 1
        identity = self.ordinal
        self.send({'jsonrpc': '2.0', 'id': identity, 'method': method, 'params': params})
        while True:
            self.frames += 1
            if self.frames > 10000: raise RuntimeError('ACP frame count bound exceeded')
            line = self.child.stdout.readline(MAX_FRAME+1)
            if not line or len(line) > MAX_FRAME or not line.endswith(b'\n'):
                raise RuntimeError('ACP frame is absent, oversized or truncated')
            row = json.loads(line)
            print(json.dumps(row), flush=True)
            if 'method' in row and 'id' in row:
                result = permission(row.get('params', {})) if row['method'] == 'session/request_permission' else None
                answer = {'jsonrpc': '2.0', 'id': row['id']}
                if result is None: answer['error'] = {'code': -32601, 'message': 'Benchmark client supports only permission callbacks'}
                else: answer['result'] = result
                self.send(answer)
            elif row.get('id') == identity and 'method' not in row:
                if 'error' in row: raise RuntimeError(request_error(method, row['error']))
                return row['result']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--product', required=True)
    parser.add_argument('--service', required=True)
    parser.add_argument('--model', required=True)
    args = parser.parse_args()
    prompt = sys.stdin.buffer.read(MAX_FRAME+1)
    if len(prompt) > MAX_FRAME: raise ValueError('Prompt bound exceeded')
    child = subprocess.Popen([args.binary, 'acp', '--embedded', '--product', args.product, '--service', args.service],
                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr)
    try:
        client = Client(child)
        client.request('initialize', {'protocolVersion': 1, 'clientCapabilities': {}})
        session = client.request('session/new', {'cwd': str(Path.cwd()), 'mcpServers': []})['sessionId']
        client.request('session/set_config_option', {'sessionId': session, 'configId': 'model', 'value': args.model})
        result = client.request('session/prompt', {'sessionId': session, 'prompt': [{'type': 'text', 'text': prompt.decode()}]})
        print(json.dumps({'type': 'benchmark.grapt.completed', 'selected_model': args.model,
                          **result, 'session': session}), flush=True)
        return 0 if result.get('stopReason') == 'end_turn' else 1
    finally:
        child.stdin.close()
        child.terminate()
        try: child.wait(timeout=5)
        except subprocess.TimeoutExpired: child.kill(); child.wait(timeout=5)
        child.stdout.close()


if __name__ == '__main__':
    sys.exit(main())
