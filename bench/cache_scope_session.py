"""Four fresh cache-scope Sessions on one embedded ACP process, inside one sandbox."""
import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import sys
import time

from opencode_compaction import NativeSession
from grapt_usage import collect

# The product's held project prompt-cache domain (grapt-provider request/cache.rs).
PROMPT_CACHE_DOMAIN = 'secode-prompt-cache-v1'  # grapt-rename: keep

ORDER = [('project-key', 1), ('project-key', 2),
         ('project-key-and-header', 1), ('project-key-and-header', 2)]
PROMPT = 'Reply only READY.'
MAX_REPLY_BYTES = 65536


def fixture_bytes(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 4096:
        raise ValueError('Cache-scope README absent or exceeds its bound')
    return path.read_bytes()


def placement(database, session):
    if database.is_symlink() or not database.is_file() or database.stat().st_size > 512*1024*1024:
        raise ValueError('Private placement database absent or outside bounds')
    with sqlite3.connect(database.absolute().as_uri()+'?mode=ro', uri=True, timeout=1) as connection:
        connection.execute('PRAGMA query_only=ON')
        rows = connection.execute('SELECT project_id,directory FROM session_list_projection '
                                  'WHERE session_id=? LIMIT 2', (session,)).fetchall()
    if len(rows) != 1 or not all(isinstance(value, str) and value for value in rows[0]):
        raise ValueError('Native placement identity missing or ambiguous')
    return {'project': rows[0][0], 'directory': rows[0][1]}


class Capture:
    def __init__(self, stream):
        self.stream = stream
        self.session = None
        self.answer = []
        self.answer_bytes = 0
        self.updates = {}

    def write(self, text):
        self.stream.write(text)
        row = json.loads(text)
        params = row.get('message', {}).get('params', {})
        if row.get('direction') == 'server' and params.get('sessionId') == self.session:
            update = params.get('update', {})
            kind = update.get('sessionUpdate')
            if kind:
                self.updates[kind] = self.updates.get(kind, 0)+1
            if kind == 'agent_message_chunk':
                chunk = update.get('content', {}).get('text', '')
                self.answer_bytes += len(chunk.encode())
                if self.answer_bytes > MAX_REPLY_BYTES:
                    raise ValueError('Cache-scope answer exceeds its bound')
                self.answer.append(chunk)

    def flush(self):
        self.stream.flush()


def probe(client, capture, service, timeout, initialize_only=False):
    """Keep the service owner alive; only Session identity and scope change."""
    root = Path.cwd().resolve(strict=True)
    readme = root/'README.md'
    seed = fixture_bytes(readme)
    config = service/'service.conf'
    if config.is_symlink() or config.stat().st_size > 1024*1024:
        raise ValueError('Private service configuration exceeds its bound')
    original = config.read_text()
    base = '\n'.join(line for line in original.splitlines()
                     if not line.startswith('prompt_cache_scope='))+'\n'
    deadline = time.monotonic()+timeout
    rows, identities, project, stopped_pairs = [], set(), None, set()
    try:
        initialized = client.request('initialize', {'protocolVersion': 1, 'clientCapabilities': {}})
        for mode, number in ORDER:
            row = {'mode': mode, 'session_ordinal': number, 'success': False,
                   'state': 'not-dispatched', 'prompt_dispatched': False}
            rows.append(row)
            if mode in stopped_pairs:
                row['reason'] = 'First Session failed its READY smoke'
                continue
            remaining = deadline-time.monotonic()
            if remaining <= 10:
                row['reason'] = 'Shared deadline reached; no extension'
                continue
            if fixture_bytes(readme) != seed:
                raise ValueError('Shared cache-scope README changed')
            client.deadline = time.monotonic()+min(180, remaining-5)
            config.write_text(base+'prompt_cache_scope='+mode+'\n')
            began = time.monotonic()
            row['session'] = client.request('session/new', {'cwd': str(root), 'mcpServers': []})['sessionId']
            if row['session'] in identities:
                raise ValueError('Native Session was reused')
            identities.add(row['session'])
            capture.session, capture.answer, capture.updates = row['session'], [], {}
            capture.answer_bytes = 0
            row['model_selection'] = client.request('session/set_config_option', {
                'sessionId': row['session'], 'configId': 'model', 'value': 'openai/gpt-6-luna'})
            row['effort_selection'] = client.request('session/set_config_option', {
                'sessionId': row['session'], 'configId': 'effort', 'value': 'medium'})
            row['placement'] = placement(service/'database.sqlite3', row['session'])
            if Path(row['placement']['directory']).resolve(strict=True) != root:
                raise ValueError('Native placement differs from shared canonical repository')
            observed = row['placement']['project']
            if project is not None and project != observed:
                raise ValueError('Native ProjectId changed before provider dispatch')
            project = observed
            digest = hashlib.sha256(b'\0'.join(value.encode() for value in [
                PROMPT_CACHE_DOMAIN, project, 'openai', 'gpt-6-luna'])+b'\0').hexdigest()
            suffix = row['session'].removeprefix('ses_')
            own = suffix if len(suffix) == 64 and all(ch in '0123456789abcdef' for ch in suffix) else row['session']
            row['expected_body_key'] = digest
            row['expected_session_header'] = own if mode == 'project-key' else digest
            row['state'] = 'session-created'
            if initialize_only:
                row['success'] = True
            else:
                sample = Path('/proc/pressure/io').read_text().splitlines()[0]
                avg = float(next(value.split('=')[1] for value in sample.split() if value.startswith('avg10=')))
                row['dispatch_preflight'] = {'io': sample, 'threshold': 0.5,
                                             'admitted': avg < 0.5, 'sampled_unix': time.time()}
                if avg >= 0.5:
                    raise ValueError('IO capacity refused before prompt; no measurement')
                row['prompt_dispatched'], row['state'] = True, 'prompt-dispatched'
                row['native_completion'] = client.prompt(row['session'], PROMPT)
                row['answer'] = ''.join(capture.answer).strip()
                row['live_updates'] = capture.updates
                row['usage'] = collect(service/'database.sqlite3', row['session'])
                calls = row['usage']['call_usage']
                row['single_call'] = len(calls) == 1 if calls is not None else None
                row['readme_unchanged'] = fixture_bytes(readme) == seed
                row['success'] = row['answer'] == 'READY' and row['readme_unchanged'] and row['single_call'] is not False
            row['wall_seconds'] = time.monotonic()-began
            if not row['success']:
                stopped_pairs.add(mode)
        return {'success': len(rows) == 4 and all(row['success'] for row in rows),
                'rows': rows, 'project': project, 'initialize': initialized}
    except (OSError, ValueError, KeyError, RuntimeError, TimeoutError, sqlite3.Error) as error:
        # A shared transport, root or placement failure stops remaining paths.
        attempted = len(rows)
        if rows:
            rows[-1].update(success=False, error=str(error))
        rows.extend({'mode': mode, 'session_ordinal': number, 'state': 'not-dispatched',
                     'success': False, 'prompt_dispatched': False, 'reason': 'Shared harness failure'}
                    for mode, number in ORDER[attempted:])
        return {'success': False, 'error': str(error), 'rows': rows, 'project': project}
    finally:
        config.write_text(original)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--initialize-only', action='store_true', help='Create Sessions without any prompts')
    parser.add_argument('--authorization-ref')
    for name in ['binary', 'product', 'service']:
        parser.add_argument('--'+name)
    parser.add_argument('--timeout', type=float, default=600)
    args = parser.parse_args()
    if not args.execute and not args.initialize_only:
        print(json.dumps({'state': 'HELD', 'provider_calls': 0, 'order': ORDER}))
        return 0
    if (args.execute == args.initialize_only or not 10 < args.timeout <= 600
            or not all([args.binary, args.product, args.service])
            or (args.execute and not args.authorization_ref)):
        parser.error('One execution mode, bounded timeout, binaries/service and live authorization required')
    capture = Capture(sys.stdout)
    client = NativeSession(args.binary, capture, args.timeout, argv=[args.binary, 'acp', '--embedded',
                           '--product', args.product, '--service', args.service])
    try:
        result = probe(client, capture, Path(args.service), args.timeout, args.initialize_only)
        result.update(type='benchmark.cache-scope.completed', authorization_ref=args.authorization_ref,
                      initialize_only=args.initialize_only, prompt=PROMPT, retries=0,
                      order_effect='Second mode inherits cache warmth; descriptive comparison only')
        print(json.dumps(result), flush=True)
        return 0 if result['success'] else 1
    finally:
        # Run this driver inside the benchmark sandbox. Its exit retires the
        # namespace and all embedded descendants, after the whole four-row part.
        client.close()


if __name__ == '__main__':
    raise SystemExit(main())
