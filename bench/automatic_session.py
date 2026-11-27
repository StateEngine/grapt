"""Fixed C04 native session; never explicitly requests compaction."""
import argparse
import ast
import base64
import hashlib
import json
import os
import select
from pathlib import Path
import subprocess
import sys
import time
import tempfile
import urllib.parse
import urllib.request

import claude_compaction
import codex_compaction
import compaction_evidence as evidence
import automatic_settings
import opencode_compaction

MAX_DOCUMENT = 8 * 1024 * 1024


def stage_snapshot(symbol):
    path = Path('app.py')
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 256*1024:
        raise ValueError('Final-stage source is absent, irregular or oversized')
    body = path.read_bytes()
    tree = ast.parse(body)
    declared = any((isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) and node.name == symbol)
                   or (isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store) and node.id == symbol)
                   or (isinstance(node, ast.alias) and (node.asname or node.name.split('.')[0]) == symbol)
                   for node in ast.walk(tree))
    return {'source_sha256': hashlib.sha256(body).hexdigest(), 'symbol': symbol, 'declared': declared}


class Log:
    def __init__(self):
        self.boundaries = []
        self.deadline = time.monotonic()+550
        self.fd = sys.stdout.fileno()
        # Native stderr shares this pipe and may enable O_NONBLOCK on it.
        os.set_blocking(self.fd, False)

    def emit(self, line):
        data = memoryview(line.encode())
        if len(data) > MAX_DOCUMENT:
            raise RuntimeError('Native evidence frame exceeds its bound')
        while data:
            remaining = self.deadline-time.monotonic()
            if remaining <= 0 or not select.select([], [self.fd], [], remaining)[1]:
                raise TimeoutError('Native evidence write time limit reached')
            try:
                written = os.write(self.fd, data)
            except (BlockingIOError, InterruptedError):
                continue
            if written <= 0:
                raise RuntimeError('Native evidence pipe closed')
            data = data[written:]

    def write(self, line):
        row = json.loads(line)
        message = row.get('message', {})
        if row.get('direction') == 'server' and (
                (message.get('method') == 'item/completed'
                and message.get('params', {}).get('item', {}).get('type') == 'contextCompaction'
                )
                or message.get('type') == 'system' and message.get('subtype') == 'compact_boundary'):
            if len(self.boundaries) >= 128:
                raise RuntimeError('Native compaction boundary census exceeded 128')
            self.boundaries.append(message)
        self.emit(line)

    def flush(self):
        pass


def archive(index, size):
    # No random values, misleading instructions or enormous tool output.
    text = ''.join(f'Archived review {index}-{row}: schema inspection completed; '
                   'historical compatibility discussion has no new requirement.\n'
                   for row in range(600))
    return (text * 2)[:size]


def read_json(path, maximum=MAX_DOCUMENT):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > maximum:
        raise ValueError('Native evidence file is absent, irregular or oversized')
    return json.loads(path.read_text())


def exported(binary, session, log):
    # ACP omits the native completion notice; export is a read, not a compact command.
    with tempfile.TemporaryFile() as output:
        subprocess.run([binary, 'export', session, '--pure'], stdout=output,
                       stderr=sys.stderr, timeout=20, check=True)
        output.seek(0)
        body = output.read(MAX_DOCUMENT+1)
    if len(body) > MAX_DOCUMENT:
        raise ValueError('OpenCode export exceeds evidence bound')
    document = json.loads(body)
    log.emit(json.dumps({'type': 'benchmark.native-export', 'document': document})+'\n')
    return evidence.opencode(document, session)


def history(service, session, owner, log):
    ready = read_json(Path(service)/'serve-ready.json', 65536)
    if ready.get('pid') != owner.pid or owner.poll() is not None:
        raise RuntimeError('SE public reader no longer names the owned service')
    endpoint = ready.get('observed_endpoint')
    parsed = urllib.parse.urlsplit(endpoint)
    if (parsed.scheme != 'http' or parsed.hostname not in {'127.0.0.1', '::1'}
            or parsed.username or parsed.password or parsed.path not in {'', '/'}
            or parsed.query or parsed.fragment or not parsed.port):
        raise ValueError('SE history endpoint is not the owned loopback root')
    credential = Path(service)/'basic.credential'
    if credential.is_symlink() or credential.stat().st_size > 4096:
        raise ValueError('SE local history credential exceeds its bound')
    secret = credential.read_text().strip()
    authorization = 'Basic '+base64.b64encode(secret.encode()).decode()
    pages, seen, cursor = [], set(), None
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    for _ in range(32):
        query = ('?cursor='+urllib.parse.quote(cursor, safe='') if cursor
                 else '?order=asc&limit=50')
        request = urllib.request.Request(endpoint.rstrip('/')+'/api/session/'+
                                         urllib.parse.quote(session, safe='')+'/message'+query,
                                         headers={'Authorization': authorization})
        try:
            with opener.open(request, timeout=10) as response:
                body = response.read(MAX_DOCUMENT+1)
        except OSError:
            raise RuntimeError('Owned SE public history read failed') from None
        if len(body) > MAX_DOCUMENT:
            raise ValueError('SE public history exceeds evidence bound')
        document = json.loads(body)
        page = {'session': session, 'body': document}
        pages.append(page)
        log.emit(json.dumps({'type': 'benchmark.grapt.history', **page})+'\n')
        cursor = document.get('cursor', {}).get('next')
        if not cursor:
            return evidence.grapt(pages, session)
        if not isinstance(cursor, str) or len(cursor) > 4096 or cursor in seen:
            raise ValueError('SE public history cursor is invalid or cyclic')
        seen.add(cursor)
    raise ValueError('SE public history page census exceeded 32')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--harness', choices=['codex', 'opencode', 'claude', 'grapt'], required=True)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--model', required=True)
    parser.add_argument('--product')
    parser.add_argument('--service')
    parser.add_argument('--trigger-profile', default='c04-32k')
    parser.add_argument('--server-side', choices=['off', 'on'], default='off')
    args = parser.parse_args()
    profile = automatic_settings.trigger_profile(args.trigger_profile, args.server_side == 'on', args.harness)
    body = sys.stdin.buffer.read(MAX_DOCUMENT+1)
    if len(body) > MAX_DOCUMENT:
        raise ValueError('Automatic task input exceeds its bound')
    task = json.loads(body)
    policy = task['automatic_compaction']
    automatic_settings.validate_policy(policy, profile)
    log, session, owner = Log(), None, None
    result = {'type': 'benchmark.automatic.completed', 'profile': 'controlled-native-automatic-compaction',
              'observed_compactions': None, 'compaction_coverage': 'unknown',
              'continued_same_session': False, 'session': None}
    result.update(profile)
    try:
        if args.harness == 'codex':
            session = codex_compaction.NativeSession(args.binary, log, 550,
                argv=[args.binary, '-c', 'model_auto_compact_token_limit=32000',
                      'app-server', '--stdio'])
            session.request('initialize', {'clientInfo': {'name': 'bench-C04', 'version': '1'},
                                          'capabilities': {'experimentalApi': True}})
            session.send({'method': 'initialized', 'params': {}})
            response = session.request('thread/start', {'model': args.model, 'cwd': str(Path.cwd()),
                'ephemeral': True, 'sandbox': 'danger-full-access', 'approvalPolicy': 'never'})
            identity = session.thread = response['thread']['id']
            prompt = session.turn
            collect = lambda: evidence.codex(log.boundaries, identity)
        elif args.harness == 'claude':
            os.environ['CLAUDE_CODE_AUTO_COMPACT_WINDOW'] = '64000'
            os.environ['CLAUDE_AUTOCOMPACT_PCT_OVERRIDE'] = '50'
            session = claude_compaction.NativeSession(args.binary, log, 550)
            prompt = session.prompt
            collect = lambda: evidence.claude(log.boundaries, session.session_id)
            identity = None
        else:
            if args.harness == 'grapt':
                if not args.product or not args.service:
                    raise ValueError('SE C04 needs an isolated product/service')
                # Own one product, use its ordinary public API for ACP and history.
                owner = subprocess.Popen([args.product, 'serve', '--service', args.service, '--port', '0'],
                                         stdout=sys.stderr, stderr=sys.stderr)
                deadline = time.monotonic()+60
                while not (Path(args.service)/'serve-ready.json').is_file():
                    if owner.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError('SE owned history service did not become ready')
                    time.sleep(0.1)
                argv = [args.binary, 'acp', '--service', args.service]
            else:
                argv = None
            session = opencode_compaction.NativeSession(args.binary, log, 550, argv=argv)
            session.request('initialize', {'protocolVersion': 1, 'clientCapabilities': {}})
            identity = session.request('session/new', {'cwd': str(Path.cwd()), 'mcpServers': []})['sessionId']
            session.request('session/set_config_option', {'sessionId': identity,
                            'configId': 'model', 'value': args.model})
            prompt = lambda text: session.prompt(identity, text)
            collect = (lambda: history(args.service, identity, owner, log)) if owner else (
                       lambda: exported(args.binary, identity, log))
        prompt(task['prompt'])
        if args.harness == 'claude':
            identity = session.session_id
        result['session'] = identity
        for index in range(policy['archive_blocks']):
            prompt('Archive review stage. Keep the initial active requirements. '
                   'Do not change files at this stage. Reply only ACK.\n'+
                   archive(index, policy['archive_block_bytes']))
        before = collect()
        result['pre_final_automatic_compactions'] = before
        symbol = policy['final_stage_symbol']
        result['stage_before_final'] = stage_snapshot(symbol)
        if result['stage_before_final']['declared']:
            raise RuntimeError('Final-stage symbol was already declared before its reveal')
        prompt(policy['final_prompt'])
        completed = collect()
        result['stage_after_final'] = stage_snapshot(symbol)
        new_work = (result['stage_after_final']['declared'] and
                    result['stage_before_final']['source_sha256'] != result['stage_after_final']['source_sha256'])
        result['new_work_after_compaction'] = bool(before) and new_work
        result.update(evidence.coverage(completed, bool(before)))
        if not result['new_work_after_compaction']:
            result['compaction_coverage'] = 'failed'
    except (OSError, ValueError, SyntaxError, KeyError, TypeError, RuntimeError, TimeoutError,
            subprocess.SubprocessError) as error:
        result['error'] = str(error)
    finally:
        if session is not None:
            session.close()
        if owner is not None:
            owner.terminate()
            try:
                owner.wait(timeout=5)
            except subprocess.TimeoutExpired:
                owner.kill()
                owner.wait(timeout=5)
    # Preserve the terminal diagnostic even when the session deadline expired.
    log.deadline = time.monotonic()+5
    log.emit(json.dumps(result)+'\n')
    return 0 if result['compaction_coverage'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
