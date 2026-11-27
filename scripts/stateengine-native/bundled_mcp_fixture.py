"""Drive the canonical bundled StateEngine launcher inside a disposable project."""
import json
from pathlib import Path
import shutil
import os
import stat
import subprocess
import sys
import socket
import hashlib


def delivered_packet(pointer, workspace):
    if pointer.get('state') != 'written':
        raise RuntimeError('canonical packet delivery pointer is not written')
    path = Path(pointer['path'])
    if not path.is_absolute() or not path.resolve().is_relative_to(Path(workspace).resolve()):
        raise RuntimeError('canonical packet path escaped its disposable workspace')
    if path.is_symlink() or not 0 < path.stat().st_size <= 1024 * 1024:
        raise RuntimeError('canonical packet file type or byte bound')
    raw = path.read_bytes()
    if len(raw) != pointer['bytes'] or hashlib.sha256(raw).hexdigest() != pointer['sha256']:
        raise RuntimeError('canonical packet bytes differ from the delivered pointer')
    return raw.decode('utf-8')


def remove_private_home(home):
    home = Path(home)
    if home.is_symlink():
        raise RuntimeError('private fixture HOME is redirected')
    corrected = 0

    def readonly(function, path, exc_info):
        nonlocal corrected
        error = exc_info[1]
        target = Path(path)
        if sys.platform != 'win32' or not isinstance(error, PermissionError):
            raise error
        if (corrected >= 256 or target.is_symlink()
                or not target.resolve().is_relative_to(home.resolve()) or not target.is_file()):
            raise RuntimeError('private readonly cleanup escaped its file bound')
        os.chmod(target, stat.S_IREAD | stat.S_IWRITE)
        corrected += 1
        function(path)

    # onerror also supports the installed Windows bench Python.
    shutil.rmtree(home, onerror=readonly)


class Fixture:
    def __init__(self, payload, environment, profile=None):
        self.payload = payload
        self.se = payload / ('se.exe' if sys.platform == 'win32' else 'se')
        self.environment = environment
        self.home = Path(environment['HOME'])
        self.profile = profile or self.cli(['init', '--project', 'grapt-bound-gate-fixture'])['profile']
        if any(not Path(self.profile[key]).resolve().is_relative_to(self.home.resolve())
               for key in ('repo_root', 'registry_root')):
            raise RuntimeError('canonical producer did not isolate the disposable issuer')
        self.binding = dict(project=self.profile['project'], agent=self.profile['agent_id'],
                            recipient=self.profile['human_id'])
        self.launched = False
        self.engine = False

    def cli(self, args, timeout=40):
        common = [] if not hasattr(self, 'profile') else [
            '--project', self.profile['project'], '--project-root', self.profile['repo_root'],
            '--registry-root', self.profile['registry_root']]
        if not hasattr(self, 'profile') and self.environment.get('STATEENGINE_REGISTRY_ROOT'):
            common = ['--registry-root', self.environment['STATEENGINE_REGISTRY_ROOT']]
        result = subprocess.run([str(self.se), *args, *common, '--json'],
                                env=self.environment, capture_output=True, timeout=timeout)
        if any(len(raw) > 1024 * 1024 for raw in (result.stdout, result.stderr)):
            raise RuntimeError('bound-MCP producer CLI output bound')
        if result.returncode:
            try:
                public = json.loads(result.stdout)
            except ValueError:
                public = None
            reason = ('detach-spawn-failed' if isinstance(public, dict)
                      and public.get('reason') == 'detach-spawn-failed' else 'producer-command-refused')
            raise RuntimeError('bound-MCP producer CLI refused ' + ' '.join(args[:2])
                               + f' (exit {result.returncode}, {reason}): '
                               + result.stderr.decode(errors='replace')[:2048])
        return json.loads(result.stdout)

    def start(self):
        with socket.socket() as reserved:
            reserved.bind(('127.0.0.1', 0))
            port = reserved.getsockname()[1]
        # Reuse the existing public engine-session organ declaration contract;
        # execute its bundled entrypoint without importing producer modules.
        organ = Path(self.profile['repo_root']) / '.stateengine/engine-session.json'
        organ.write_text(json.dumps({'organs': ['remote-seat-mail'], 'remote-seat-mail': {
            'command': str(self.payload / ('node.exe' if sys.platform == 'win32' else 'node')), 'args': [
                str(self.payload / 'engine/src/remote-seat-mail-door.mjs'),
                '--root', self.profile['repo_root'], '--project', self.profile['project'],
                '--target-project', 'secode', '--port', str(port)],
            'healthUrl': f'http://127.0.0.1:{port}/health'}}))
        self.mail_endpoint = f'http://127.0.0.1:{port}'
        if sys.platform == 'win32':
            directories = {key: self.environment.get(key) for key in
                ('HOME', 'USERPROFILE', 'STATEENGINE_REGISTRY_ROOT',
                 'STATEENGINE_OWNER_ROOT', 'STATEENGINE_KEYS_DIR')}
            if any(not value or not Path(value).resolve().is_relative_to(self.home.resolve())
                   for value in directories.values()):
                raise RuntimeError('Windows fixture isolation paths escaped its private HOME')
            if Path(self.profile['registry_root']).resolve() != Path(directories['STATEENGINE_REGISTRY_ROOT']).resolve():
                raise RuntimeError('Windows fixture registry differs from its explicit private registry')
            evidence = dict(directories=directories, project=self.profile['project'],
                repo_root=self.profile['repo_root'], registry_root=self.profile['registry_root'],
                bundled_se=str(self.se), mail_endpoint=self.mail_endpoint)
            (Path(self.environment['TMPDIR']).parent / 'isolation-preflight.json').write_text(
                json.dumps(evidence, indent=2))
        self.engine = True
        self.cli(['up', '--detach', '--no-follow'], timeout=120)

    def declaration(self):
        return {'type': 'local', 'codemode': True, 'enabled': True,
            'command': [str(self.se), 'mcp', '--project', self.profile['project'],
                        '--root', self.profile['repo_root'], '--registry-root', self.profile['registry_root']],
            'environment': dict(self.environment, STATEENGINE_PROJECT_ID=self.binding['project'],
                                STATEENGINE_AGENT_ID=self.binding['agent'])}

    def launch(self, journey, tui):
        self.environment.update(journey.environment)
        config = journey.root / 'seat-config.json'
        config.write_text(json.dumps({'project': self.profile['project'],
            'projectRoot': self.profile['repo_root'], 'seats': [{
                'agent_id': self.profile['agent_id'], 'surface': 'grapt',
                'command': str(journey.args.cli), 'grapt_tui_binary': str(tui),
                'grapt_service': str(journey.profile),
                'grapt_workspace': self.profile['repo_root'],
                'grapt_agent': 'agt_live_headless1', 'model': 'openai/gpt-live'}]}))
        receipt = self.cli(['seat', 'launch', self.profile['agent_id'], '--surface', 'grapt',
                            '--machine', 'local', '--config', str(config)], timeout=120)
        self.launched = True
        (journey.root / 'launch-receipt.json').write_text(json.dumps(receipt, indent=2))
        rows = journey.api('GET', '/api/session')['data']
        if len(rows) != 1:
            raise RuntimeError('canonical launcher did not create exactly one Session')
        self.session = rows[0]['id']
        # Read back the producer's provisioned instruction through the public
        # Session API; launch receipts do not publish a packet pointer.
        entries = journey.api('GET', '/api/session/' + self.session + '/instructions/entries')['data']
        pointers = [row['value'] for row in entries if row.get('key') == 'stateengine-packet']
        if len(pointers) != 1:
            raise RuntimeError('canonical Session lacks its unique delivered packet pointer')
        packet = delivered_packet(pointers[0], self.profile['repo_root'])
        self.packet = next(line.split(': ', 1)[1] for line in packet.splitlines()
                           if line.startswith('packet_id: '))

    def seed(self):
        for index in range(20):
            self.cli(['msg', 'send', '--user-id', self.profile['human_id'],
                      '--to', self.profile['agent_id'], '--kind', 'done',
                      '--body', 'Disposable bound-MCP row ' + str(index) + ': ' + 'x' * 2400])
        return {'seeded': True}

    def verify(self, msg_id, marker):
        inbox = self.cli(['msg', 'inbox', '--seat', self.profile['human_id'], '--full', '--limit', '20'])
        matching = [row for row in inbox['messages'] if row['msg_id'] == msg_id]
        return {'verified': len(matching) == 1 and matching[0]['body'] == marker
                and matching[0]['from_seat'] == self.profile['agent_id']
                and matching[0]['to_seat'] == self.profile['human_id']}

    def stop_seat(self):
        if self.launched:
            self.launched = False
            self.cli(['seat', 'stop', self.profile['agent_id']], timeout=30)

    def close(self):
        try:
            self.stop_seat()
        finally:
            try:
                if self.engine:
                    self.cli(['down'], timeout=30)
            finally:
                # This exclusively created HOME contains only the disposable
                # issuer. No capability enters argv or harness evidence files.
                remove_private_home(self.home)
