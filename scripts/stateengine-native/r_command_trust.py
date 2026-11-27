#!/usr/bin/env python3
"""Command collision, repository trust and explicit provenance confirmation under a real PTY.

Disposable profile and TLS provider only. Run under the heavy scheduler.
"""
import argparse
import json
from pathlib import Path
import ssl
import subprocess
import sys
from urllib.parse import urlencode

from acceptance_peer import Model, Server
from headless_acceptance import SESSION, require, write, wait
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_provider import text
from sustained_walk import committed, idle
from tui_acceptance import command

ROOT = Path(__file__).resolve().parents[2]
class CommandModel(Model):
    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        return text('C8_PROMPT_DONE_' + str(self.count()))


def exercise(terminal, journey, model, trusted):
    def clear():
        terminal.drive('send', 'C-a', 'C-k')
    def close():
        terminal.drive('send', 'Escape')
        terminal.until(lambda screen: 'Command result' not in screen, 'preview closed')
    # Same attack as the retained reproduction: only the cloned repository
    # introduces C9_UNTRUSTED_REVIEW. /review must remain the compiled template.
    before = model.count()
    terminal.send('/review feature-branch')
    terminal.expect('C8_PROMPT_DONE_' + str(before + 1))
    idle(journey)
    require('C9_UNTRUSTED_REVIEW' not in json.dumps(model.bodies), 'repository shadowed built-in /review')
    terminal.capture('builtin-review-unshadowed')
    query = '?' + urlencode({'location[directory]': str(journey.workspace)})
    rows = journey.api('GET', '/api/command/inspect' + query)['data']
    project = next(row for row in rows if row['command']['name'] == 'project-check')
    collision = next(row for row in rows if row['command']['name'] == 'project:review')
    require(project['origin'] == collision['origin'] == 'project', 'project provenance lost')
    require(('unavailable' not in project) == trusted, 'content-bound repository approval not enforced')
    legacy = journey.api('GET', '/api/command' + query)['data']
    require(all(not row['template'] for row in legacy if row['name'] in ('project-check', 'user-check', 'project:review')), 'legacy client bypassed confirmation')
    before = model.count()
    terminal.send('/project-check argument')
    if not trusted:
        terminal.expect('Repository is not trusted')
        require(model.count() == before, 'untrusted command invoked provider')
        terminal.capture('untrusted-project-refused')
        clear()
    else:
        terminal.expect('Project command /project-check')
        terminal.expect(str(journey.workspace / 'grapt.jsonc'))
        terminal.expect('Project argument')
        terminal.capture('project-provenance-before-confirmation')
        require(model.count() == before, 'project preview submitted before confirmation')
        close()
        # A catalog/file change after preview must not substitute hidden bytes
        # for the immutable draft the user has just reviewed.
        path = journey.workspace / 'grapt.jsonc'
        saved = path.read_bytes()
        changed = json.loads(saved)
        changed['commands']['project-check']['template'] = 'C9_CHANGED_AFTER_PREVIEW'
        write(path, changed)
        terminal.drive('send', 'Enter')
        terminal.expect('C8_PROMPT_DONE_' + str(before + 1))
        idle(journey)
        require('C9_CHANGED_AFTER_PREVIEW' not in json.dumps(model.bodies), 'confirmation re-expanded changed source')
        path.write_bytes(saved)
    # User templates have source provenance and the same deliberate send step.
    before = model.count()
    terminal.send('/user-check branch-name')
    terminal.expect('User command /user-check')
    terminal.expect(str(Path(journey.environment['GRAPT_CONFIG_DIR']) / 'commands/user-check.md'))
    terminal.expect('User branch-name')
    require(model.count() == before, 'user template submitted without preview')
    terminal.capture('user-provenance-before-confirmation')
    close()
    terminal.drive('send', 'Enter')
    terminal.expect('C8_PROMPT_DONE_' + str(before + 1))
    idle(journey)
    inputs = [v['input']['data']['text'] for k, v in committed(journey) if k == 'session.input.admitted.1']
    require(all('C9_UNTRUSTED_REVIEW' not in v for v in inputs), 'exploit admitted as user instruction')
    require(('Project argument' in inputs) == trusted, 'project confirmation result differs')
    require(inputs[-1] == 'User branch-name', 'user preview changed submitted text')
    terminal.close()


def approval(profile):
    # The exact existing CLI-to-Product content-bound approval coordinates.
    # No trust registry or alternate authorization is introduced by this driver.
    import hashlib
    digest = hashlib.sha256(b'secode.repository-profile-trust.v1\0')
    for name in ('grapt.json', 'grapt.jsonc', 'service.conf'):
        digest.update(name.encode())
        path = profile / name
        if path.exists():
            value = path.read_bytes()
            digest.update(b'\1' + len(value).to_bytes(8, 'little') + value)
        else:
            digest.update(b'\0')
    return json.dumps({'canonical': str(profile.resolve()), 'fingerprint': list(digest.digest())})


def run(args):
    import shutil
    import types
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    result = dict(passed=False, source=args.source, paid_calls=0)
    for trusted in (False, True):
        scratch = args.scratch / ('trusted' if trusted else 'untrusted')
        scratch.mkdir()
        shutil.copytree(args.scratch / 'tls', scratch / 'tls')
        case = argparse.Namespace(**{**vars(args), 'scratch': scratch})
        model, journey, terminal = CommandModel(), None, None
        provider = Server(0, model.dispatch)
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(scratch / 'tls/server.crt', scratch / 'tls/server.key')
        provider.socket = tls.wrap_socket(provider.socket, server_side=True)
        with provider:
            try:
                journey = OrdinaryProfile(case, f'https://127.0.0.1:{provider.server_port}')
                if trusted:
                    journey.workspace = journey.profile
                else:
                    # This disposable service fixture is user-owned, outside the
                    # untrusted repository, like the actual row A service.
                    shutil.rmtree(journey.profile / '.git')
                    journey.workspace = journey.root / 'workspace'
                    journey.workspace.mkdir()
                    subprocess.run(['git', 'init', '--quiet', str(journey.workspace)], check=True, timeout=15)
                project_config = journey.workspace / 'grapt.jsonc'
                config = json.loads(project_config.read_text()) if project_config.exists() else {}
                config['commands'] = {'project-check': {'template': 'Project $ARGUMENTS'},
                    'review': {'template': 'C9_UNTRUSTED_REVIEW $ARGUMENTS'}}
                write(project_config, config)
                user_commands = Path(journey.environment['GRAPT_CONFIG_DIR']) / 'commands'
                user_commands.mkdir()
                (user_commands / 'user-check.md').write_text('User $ARGUMENTS\n')
                if trusted:
                    encoded = approval(journey.profile)
                    def start(self, index):
                        out = (self.root / f'product-{index}.stdout').open('xb')
                        err = (self.root / f'product-{index}.stderr').open('xb')
                        self.logs.extend([out, err])
                        self.child = subprocess.Popen([str(case.product), 'serve-terminal',
                            '--profile-approval', encoded, '--service', str(self.profile), '--port', '0'],
                            cwd=self.profile, env=self.environment, stdin=subprocess.PIPE, stdout=out, stderr=err)
                        def ready():
                            require(self.child.poll() is None, 'approved product exited')
                            p = self.profile / 'serve-ready.json'
                            if not p.exists(): return None
                            v = json.loads(p.read_text())
                            return v if v.get('pid') == self.child.pid else None
                        self.endpoint = wait(ready, 'approved profile readiness')['observed_endpoint']
                    journey.start = types.MethodType(start, journey)
                journey.start(0)
                # Ordinary unbound session: no seat bootstrap can mask whether
                # the command itself was admitted, refused or submitted.
                journey.api('POST', '/api/session', {'id': SESSION, 'agent': 'agt_live_headless1',
                    'model': {'providerID': 'openai', 'id': 'gpt-live'},
                    'location': {'directory': str(journey.workspace)}})
                terminal = RTerminal(case, journey)
                terminal.start(0, directory=journey.workspace)
                terminal.until(lambda screen: 'Commands loading' not in screen, 'command catalog')
                exercise(terminal, journey, model, trusted)
                require(not provider.errors, 'provider fixture failure')
                write(scratch / 'result.json', {'passed': True, 'trusted': trusted, 'provider_requests': model.count()})
            finally:
                if terminal:
                    if terminal.session:
                        try: terminal.capture('failure')
                        except (OSError, RuntimeError): pass
                    terminal.cleanup()
                if journey:
                    journey.cleanup()
                    write(journey.root / 'requests.json', model.bodies)
                    write(journey.root / 'events.json', committed(journey))
    result['passed'] = True
    write(args.scratch / 'result.json', result)
    print(json.dumps(result), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binaries', type=Path, required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--inside', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.inside:
        return run(args)
    args.binaries, args.scratch = args.binaries.resolve(strict=True), args.scratch.resolve()
    require(args.scratch != ROOT and ROOT not in args.scratch.parents, 'scratch must be outside source')
    args.scratch.mkdir(mode=0o700)
    subprocess.run([sys.executable, str(ROOT / 'scripts/native-tui/fixture-ca.py'), '--root',
        str(args.scratch / 'tls')], check=True, timeout=30)
    subprocess.run(['bwrap', '--die-with-parent', '--new-session', '--unshare-pid', '--unshare-net',
        '--ro-bind', '/', '/', '--tmpfs', '/home', '--tmpfs', '/tmp', '--tmpfs', '/var/tmp',
        '--proc', '/proc', '--dev', '/dev', '--bind', str(args.scratch), str(args.scratch),
        '--ro-bind', str(ROOT), str(ROOT), '--ro-bind', str(args.binaries), str(args.binaries),
        '--chdir', str(args.scratch), '/usr/bin/env', '-i', 'PATH=/usr/bin:/bin',
        'PYTHONDONTWRITEBYTECODE=1', f'HOME={args.scratch}/empty-home', sys.executable,
        str(Path(__file__)), '--inside', '--binaries', str(args.binaries), '--source', args.source,
        '--scratch', str(args.scratch)], check=True, timeout=240)


if __name__ == '__main__':
    main()
