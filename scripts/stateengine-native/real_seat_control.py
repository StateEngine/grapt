"""Own only a fresh fixture root and the candidate's public lifecycle commands."""
import base64
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import time
import urllib.request

from headless_acceptance import service_helper


def private_environment(root):
    names = ('PATH', 'SystemRoot', 'WINDIR', 'COMSPEC', 'PATHEXT')
    environment = {name: os.environ[name] for name in names if name in os.environ}
    for key, directory in (
        ('HOME', 'home'), ('USERPROFILE', 'home'), ('APPDATA', 'home/roaming'),
        ('LOCALAPPDATA', 'home/local'), ('TEMP', 'tmp'), ('TMP', 'tmp'),
        ('GRAPT_TEST_HOME', 'product-home'), ('GRAPT_CONFIG_DIR', 'product-config'),
    ):
        path = root / directory
        path.mkdir(parents=True, exist_ok=True)
        environment[key] = str(path)
    environment.update(PYTHONUTF8='1', TOKIO_WORKER_THREADS='2')
    return environment


def fresh_root(path):
    path = Path(path).absolute()
    if path.parent != Path('E:/temp') or not re.fullmatch(r'real-seat-[a-zA-Z0-9_-]{1,80}', path.name):
        raise ValueError('use a fresh private E:/temp/real-seat-* root')
    for parent in (path.parent, *path.parent.parents):
        if parent.is_symlink() or getattr(parent, 'is_junction', lambda: False)():
            raise ValueError('fixture root ancestors must not redirect')
    path.mkdir()
    sid_output = subprocess.check_output(['whoami', '/user', '/fo', 'csv', '/nh'],
                                          timeout=10, text=True)
    sid = re.findall(r'S-1-5-(?:[0-9]+-)*[0-9]+', sid_output)
    if len(sid) != 1:
        raise ValueError('cannot identify the fixture directory owner')
    subprocess.run(['icacls', str(path), '/inheritance:r', '/grant:r',
                    '*' + sid[0] + ':(OI)(CI)F', '*S-1-5-18:(OI)(CI)F',
                    '*S-1-5-32-544:(OI)(CI)F'], check=True,
                   capture_output=True, timeout=10)
    return path


class Commands:
    def __init__(self, root, checkout, node, journal, deadline):
        self.root, self.checkout, self.journal = root, checkout, journal
        self.deadline = deadline
        self.environment = private_environment(root)
        self.entry = [str(node), str(checkout / 'bin/se.mjs')]
        self.project = 'seat-witness-' + root.name.removeprefix('real-seat-').lower()
        self.workspace, self.registry = root / 'workspace', root / 'registry'
        self.binding = ['--project', self.project, '--root', str(self.workspace),
                        '--registry-root', str(self.registry)]

    def guard(self):
        if time.monotonic() >= self.deadline:
            raise TimeoutError('seat witness deadline exhausted')
        for name in ('product.stdout', 'product.stderr'):
            path = self.root / name
            if path.exists() and path.stat().st_size > 2 * 1024 * 1024:
                raise ValueError(name + ' exceeded 2 MiB')

    def wait(self, read, description, seconds=20):
        ends = min(self.deadline, time.monotonic() + seconds)
        while time.monotonic() < ends:
            self.guard()
            result = read()
            if result:
                return result
            time.sleep(0.05)
        raise TimeoutError('did not observe ' + description)

    def run(self, name, argv, *, timeout=90, required=True, cleanup=False):
        remaining = timeout if cleanup else min(timeout, self.deadline - time.monotonic())
        if remaining <= 0:
            raise TimeoutError('seat witness deadline exhausted')
        paths = [self.root / (name + '.' + suffix) for suffix in ('stdout', 'stderr')]
        with paths[0].open('xb') as out, paths[1].open('xb') as err:
            process = subprocess.Popen(list(map(str, argv)), cwd=self.root,
                                       env=self.environment, stdin=subprocess.DEVNULL,
                                       stdout=out, stderr=err)
            try:
                ends = time.monotonic() + remaining
                while process.poll() is None:
                    if not cleanup:
                        self.guard()
                    if time.monotonic() >= ends:
                        raise TimeoutError(name + ' exceeded its command deadline')
                    if any(path.stat().st_size > 2 * 1024 * 1024 for path in paths):
                        raise ValueError('public command output exceeded 2 MiB')
                    time.sleep(0.02)
            except BaseException:
                process.kill()
                process.wait(timeout=5)
                raise
        if any(path.stat().st_size > 2 * 1024 * 1024 for path in paths):
            raise ValueError('public command output exceeded 2 MiB')
        self.journal.record('command_finished', command=name, exit_code=process.returncode)
        if required and process.returncode:
            raise RuntimeError(name + ' refused; see its bounded stderr log')
        return process.returncode, paths[0].read_text(encoding='utf-8')

    def se(self, name, arguments, **options):
        return self.run(name, [*self.entry, *arguments], **options)

    def prepare_mcp_path(self, agent):
        """StateEngine's supported path-only API; never issues a Session or config."""
        arguments = dict(module=str(self.checkout / 'src/secode-launch-prep.mjs'),
                         project=self.project, seat=agent,
                         roots=[str(self.workspace)], registry=str(self.registry))
        code = ("import {pathToFileURL} from 'node:url';"
                "const a=JSON.parse(process.argv[1]);"
                "const m=await import(pathToFileURL(a.module));"
                "const v=await m.prepareSecodeMcpConfigPath({project:a.project,seatId:a.seat,"
                "projectRoots:a.roots,spawnEnv:{...process.env,STATEENGINE_REGISTRY_ROOT:a.registry}});"
                "process.stdout.write(JSON.stringify(v));")
        _, raw = self.run('mcp-path', [self.entry[0], '--input-type=module', '-e', code,
                                     json.dumps(arguments)], timeout=15)
        path = Path(json.loads(raw)['path']).resolve()
        if not path.is_relative_to(self.root.resolve()) or path.exists():
            raise ValueError('path-only preparation must name an absent private config')
        self.environment.update(GRAPT_CONFIG=str(path), OPENCODE_CONFIG=str(path))
        self.journal.record('generated_config_path_prepared', path=str(path),
                            api='prepareSecodeMcpConfigPath', session_issued=False)
        return path

    def declare_mail(self, node):
        """The existing engine-session organ contract, supplied by StateEngine 5b."""
        path = self.workspace / '.stateengine/engine-session.json'
        if path.stat().st_size > 65536:
            raise ValueError('private engine organ declaration exceeded 64 KiB')
        value = json.loads(path.read_text(encoding='utf-8'))
        organs = value['organs']
        if not isinstance(organs, list) or not all(isinstance(name, str) for name in organs):
            raise ValueError('private engine organs must be named explicitly')
        with socket.socket() as reserved:
            reserved.bind(('127.0.0.1', 0))
            port = reserved.getsockname()[1]
        if port == 4877:
            raise ValueError('private fixture cannot use the shared native mail port')
        if 'remote-seat-mail' not in organs:
            organs.append('remote-seat-mail')
        value['remote-seat-mail'] = dict(command=str(node), args=[
            str(self.checkout / 'src/remote-seat-mail-door.mjs'), '--root', str(self.workspace),
            '--project', self.project, '--target-project', 'secode', '--port', str(port)],
            healthUrl=f'http://127.0.0.1:{port}/health')
        path.write_text(json.dumps(value, indent=2) + '\n', encoding='utf-8')
        self.journal.record('private_mail_organ_declared', endpoint=f'http://127.0.0.1:{port}')
        return port


def stop_engine(control):
    """A private shutdown must include its owner and observe all reported PIDs gone."""
    import psutil
    deadline = time.monotonic() + 45
    _, raw = control.se('down', ['down', *control.binding, '--including-owner',
        '--timeout-ms', '35000', '--json'], timeout=45, cleanup=True)
    stopped = json.loads(raw)
    if (stopped.get('action') != 'stopped' or stopped.get('project') != control.project
            or stopped.get('orphans') != [] or stopped.get('stayed_up') != []
            or stopped.get('identity_check', {}).get('status') not in ('verified', 'absent')
            or stopped.get('shared_owner', {}).get('before', {}).get('status') != 'included'):
        raise ValueError('private engine shutdown did not confirm its included owner')
    rows = stopped.get('organs')
    if not isinstance(rows, list):
        raise ValueError('private engine shutdown omitted its process observations')
    owner = stopped.get('session_pid')
    if type(owner) is not int or owner <= 0:
        raise ValueError('private engine shutdown omitted its owner process')
    rows = [*rows, {'pid': owner}]
    if 'owner_stop' in stopped:
        rows.append(stopped['owner_stop'])
    for name in ('launcher_stops', 'listener_stops'):
        rows.extend(stopped.get(name, []))
    if len(rows) > 32:
        raise ValueError('private engine shutdown exceeded its process bound')
    pids = {row.get('pid') for row in rows if row.get('pid') is not None}
    if not pids or any(type(pid) is not int or pid <= 0 for pid in pids):
        raise ValueError('private engine shutdown returned invalid process identities')
    while any(psutil.pid_exists(pid) for pid in pids):
        if time.monotonic() >= deadline:
            raise TimeoutError('private engine processes survived the cleanup deadline')
        time.sleep(min(.05, max(0, deadline - time.monotonic())))
    if time.monotonic() >= deadline:
        raise TimeoutError('private engine cleanup deadline exhausted')
    control.journal.record('private_engine_stopped', processes_gone=True)


def api(endpoint, method, route, body=None):
    auth = base64.b64encode(f'{service_helper.USERNAME}:{service_helper.SECRET}'.encode()).decode()
    request = urllib.request.Request(endpoint + route, method=method,
        data=None if body is None else json.dumps(body).encode(),
        headers={'Authorization': 'Basic ' + auth, 'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=5) as response:
        data = response.read(2 * 1024 * 1024 + 1)
    if len(data) > 2 * 1024 * 1024:
        raise ValueError('product API observation exceeded 2 MiB')
    return json.loads(data) if data else None


def find_session(endpoint):
    rows = api(endpoint, 'GET', '/api/session')['data']
    if len(rows) != 1:
        raise ValueError('real launch must create exactly one private product Session')
    return rows[0]['id']
