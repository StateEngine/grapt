#!/usr/bin/env python3
"""Ordinary R permission selection, refusal and allowed file effects under a real PTY.

Disposable profile and TLS provider only. Run under the heavy scheduler.
"""
import argparse
import json
from pathlib import Path
import ssl
import subprocess
import sys

from acceptance_peer import Model, Server, PROJECT, AGENT, CAPABILITY
from headless_acceptance import SESSION, require, write, wait
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_provider import text, calls, output
from sustained_walk import committed, idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]
PATCH = '*** Begin Patch\n*** Update File: command-proof.txt\n@@\n-before command edit\n+after command edit\n*** End Patch'


class PermissionModel(Model):
    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        count = self.count()
        if count in (1, 3):
            identity = 'call_permission_denied' if count == 1 else 'call_permission_allowed'
            return calls([(identity, 'apply_patch', PATCH)])
        if count == 2:
            rows = [row for row in body.get('input', [])
                    if row.get('call_id') == 'call_permission_denied'
                    and row.get('type') == 'custom_tool_call_output']
            require(len(rows) == 1 and rows[0]['output'] ==
                    "Tool failed: AuthorityUnavailable\nfs.modify is denied by this Session's inspect permission preset; do not repeat the call",
                    'inspect did not deliver its exact denial to the provider')
            return text('PERMISSION_WRITE_BLOCKED')
        require(count == 4, 'unexpected permission fixture request')
        result = output(body, 'call_permission_allowed')
        require('error' not in json.dumps(result).lower(), 'author patch failed')
        return text('PERMISSION_WRITE_ALLOWED')


def exercise(terminal, journey, model):
    def close_pane():
        terminal.drive('send', 'Escape')
        terminal.until(lambda screen: 'Command result' not in screen, 'permissions pane closed')

    def local(value, expected, label):
        before = model.count()
        terminal.send(value)
        terminal.expect(expected)
        terminal.capture(label)
        require(model.count() == before, value + ' invoked a provider')

    def policy():
        return journey.api('GET', '/api/session/' + SESSION + '/preset')['data']

    local('/permissions', 'PERMISSIONS', 'initial-permissions')
    original = policy()
    close_pane()
    for value in ['/permissions missing', '/permissions author extra', '/permissions bypass nope']:
        local(value, 'Use /permissions', 'invalid-permission')
        require(policy() == original, 'invalid arguments changed permissions')
        terminal.drive('send', 'C-a', 'C-k')
    local('/permissions bypass', 'Bypass disables permission checks', 'bypass-needs-confirmation')
    require(policy() == original, 'bypass applied without confirmation')
    terminal.drive('send', 'C-a', 'C-k')
    local('/permissions inspect', 'Preset: inspect', 'inspect-selected')
    require(policy()['selected'] == policy()['effective'] == 'inspect', 'inspect not effective')
    close_pane()
    proof = journey.workspace / 'command-proof.txt'
    terminal.send('Attempt the requested fixture file edit.')
    terminal.expect('PERMISSION_WRITE_BLOCKED')
    idle(journey)
    failures = [data['outcome'] for kind, data in committed(journey)
                if kind == 'session.tool.terminal.1' and data.get('call') == 'call_permission_denied']
    require(len(failures) == 1 and failures[0]['kind'] == 'failed', 'denied tool has no terminal failure')
    detail = failures[0]['detail']['id']
    reason = (journey.profile / 'dogfood-artifacts/objects' / detail[:2] / detail).read_text()
    require(reason == "fs.modify is denied by this Session's inspect permission preset; do not repeat the call",
            'tool failed for a reason other than inspect policy: ' + reason)
    require(model.count() == 2, 'inspect denial did not complete its linked response')
    require(proof.read_text() == 'before command edit\n', 'inspect allowed the write')
    terminal.capture('inspect-write-refused')
    terminal.drive('send', 'C-a', 'C-k')
    local('/permissions author', 'Preset: author', 'author-selected')
    require(policy()['selected'] == policy()['effective'] == 'author', 'author not effective')
    close_pane()
    terminal.send('Apply the same requested fixture file edit.')
    terminal.expect('PERMISSION_WRITE_ALLOWED')
    idle(journey)
    require(proof.read_text() == 'after command edit\n', 'author did not apply the allowed edit')
    terminal.capture('author-write-applied')
    local('/permissions bypass confirm', 'BYPASS: permission checks disabled', 'explicit-bypass')
    require(policy()['selected'] == policy()['effective'] == 'bypass', 'explicit bypass not selected')
    close_pane()
    terminal.expect('BYPASS: permission checks disabled')
    command(['tmux', 'resize-window', '-t', terminal.session, '-x', '40', '-y', '16'])
    terminal.expect('BYPASS: permission checks disabled')
    terminal.capture('bypass-closed-pane-narrow')
    command(['tmux', 'resize-window', '-t', terminal.session, '-x', '160', '-y', '45'])
    local('/status', 'Usage · live session', 'bypass-status-update')
    terminal.drive('send', 'Escape')
    terminal.until(lambda screen: 'Usage · live session' not in screen, 'status pane closed')
    terminal.expect('BYPASS: permission checks disabled')
    pane = terminal.session
    journey.stop()
    terminal.until(lambda screen: 'reconnect' in screen.lower() or 'Connecting' in screen,
                   'offline service notice')
    require('BYPASS' in terminal.capture('bypass-offline'), 'offline update hid the last known bypass')
    journey.start(1)
    terminal.until(lambda screen: 'Connected' in screen and 'BYPASS: permission checks disabled' in screen,
                   'same R reconnect with server bypass policy')
    require(terminal.session == pane and policy()['effective'] == 'bypass', 'reconnect replaced the terminal or policy')
    terminal.capture('bypass-reconnected')
    local('/permissions maintainer', 'Preset: maintainer', 'maintainer-selected')
    require(policy()['selected'] == policy()['effective'] == 'maintainer', 'maintainer not effective')
    close_pane()
    terminal.send('/quit')
    wait(lambda: command(['tmux', 'display-message', '-p', '-t', terminal.session,
         '#{pane_dead} #{pane_dead_status}']).strip() == '1 0', 'slash quit clean exit')
    terminal.drive('stop')
    terminal.session = None
    require(model.count() == 4, 'permission commands admitted extra model input')


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    model, mail = PermissionModel(), TuiMail()
    provider = Server(0, model.dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    journey, terminal = None, None
    result = dict(passed=False, source=args.source, paid_calls=0, windows='not measured')
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            journey.workspace = journey.root / 'workspace'
            journey.workspace.mkdir()
            subprocess.run(['git', 'init', '--quiet', str(journey.workspace)], check=True, timeout=15)
            proof = journey.workspace / 'command-proof.txt'
            proof.write_text('before command edit\n')
            command(['git', '-C', journey.workspace, 'add', '--', 'command-proof.txt'])
            committed_fixture = subprocess.run(['git', '-C', str(journey.workspace), '-c',
                'user.name=Fixture', '-c', 'user.email=fixture@invalid', 'commit', '--quiet',
                '-F', '-', '--', 'command-proof.txt'], input='command fixture\n', text=True,
                capture_output=True, timeout=10)
            require(committed_fixture.returncode == 0, 'fixture baseline commit failed')
            config_path = journey.profile / 'grapt.jsonc'
            config = json.loads(config_path.read_text())
            config['compaction']['auto'] = False
            write(config_path, config)
            journey.start(0)
            identity = {'scope': 'project', 'project_id': PROJECT, 'agent_id': AGENT,
                        'mail_transport_protocol': 'remote-seat-mail-transport/v2'}
            provision = journey.cli(['session', 'provision', '--service', str(journey.profile),
                '--session', SESSION, '--agent', 'agt_live_headless1', '--model', 'openai/gpt-live',
                '--location', str(journey.workspace), '--identity-entry', json.dumps(identity),
                '--packet-entry', json.dumps(journey.pointer)], 'provision', (CAPABILITY+'\n').encode())
            require(provision.returncode == 0, 'fixture provision failed')
            terminal = RTerminal(args, journey)
            terminal.start(0, directory=journey.workspace)
            terminal.until(lambda screen: 'Commands loading' not in screen, 'command catalog')
            exercise(terminal, journey, model)
            require(not provider.errors and not peer.errors, 'fixture peer failure')
            result.update(passed=True, provider_requests=model.count(), inspect_denied=True, author_allowed=True, bypass_confirmation=True)
        finally:
            if terminal:
                if terminal.session:
                    try:
                        terminal.capture('failure')
                    except (OSError, RuntimeError):
                        pass
                terminal.cleanup()
            if journey:
                journey.cleanup()
                write(journey.root / 'requests.json', model.bodies)
                write(journey.root / 'events.json', committed(journey))
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
