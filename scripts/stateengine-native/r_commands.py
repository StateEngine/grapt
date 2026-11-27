#!/usr/bin/env python3
"""Ordinary R slash commands, file undo and conversation rewind under a real PTY.

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
EDIT = 'C8_EDIT_FILE'
PATCH = '*** Begin Patch\n*** Update File: command-proof.txt\n@@\n-before command edit\n+after command edit\n*** End Patch'


class CommandModel(Model):
    def __init__(self):
        super().__init__()
        self.edited = False

    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        encoded = json.dumps(body)
        if 'call_c8_patch' in encoded:
            result = output(body, 'call_c8_patch')
            require('error' not in json.dumps(result).lower(), 'patch tool failed')
            return text('C8_EDIT_DONE')
        if EDIT in encoded:
            require(not self.edited, 'edit was unexpectedly replayed')
            self.edited = True
            return calls([('call_c8_patch', 'apply_patch', PATCH)])
        return text('C8_PROMPT_DONE_' + str(self.count()))


def exercise(terminal, journey, model):
    def local(value, expected):
        before = model.count()
        terminal.send(value)
        terminal.expect(expected)
        terminal.capture(value.split()[0].lstrip('/') or 'command')
        require(model.count() == before, value + ' invoked a provider')

    def clear():
        terminal.drive('send', 'C-a', 'C-k')

    def close_pane():
        terminal.drive('send', 'Escape')
        terminal.until(lambda screen: 'Command result' not in screen, 'command pane closed')

    local('/rename C8 command session', 'Session renamed.')
    require(journey.api('GET', '/api/session/' + SESSION)['data']['title'] == 'C8 command session',
            'rename was not durable')
    local('/not-a-command retained', 'Unknown command')
    clear()
    def prompt(value):
        expected = 'C8_PROMPT_DONE_' + str(model.count() + 1)
        terminal.send(value)
        terminal.expect(expected)
        idle(journey)

    prompt('//not-a-command literal')
    prompt('/project-check "a b.rs" second third')
    prompt('/user-check branch-name')
    prompt('/review')
    prompt('/review feature-branch')
    terminal.send(EDIT)
    terminal.expect('C8_EDIT_DONE')
    idle(journey)
    changed = journey.workspace / 'command-proof.txt'
    require(changed.read_text() == 'after command edit\n', 'tool edit missing')
    local('/undo', 'Use /undo apply')
    require(changed.read_text() == 'after command edit\n', 'undo preview modified files')
    close_pane()
    local('/undo apply', 'FILE UNDO')
    require(changed.read_text() == 'before command edit\n', 'undo did not restore tool edit')
    close_pane()
    before = model.count()
    terminal.send('/rewind')
    terminal.expect('Conversation rewound; files unchanged.')
    terminal.expect('C8_PROMPT_DONE_5')
    screen = terminal.capture('rewind-reopened-prompt')
    require('History refresh failed' not in screen, 'rewind did not refresh retained history')
    require('C8_EDIT_DONE' not in screen, 'rewind retained the removed exchange')
    require(EDIT in '\n'.join(screen.splitlines()[-8:]), 'rewind did not reopen the selected prompt')
    require(changed.read_text() == 'before command edit\n', 'rewind changed files')
    require(model.count() == before, 'rewind replayed the model')
    # Exit is local and must not submit the reopened prompt or reach a provider.
    clear()
    terminal.send('/quit')
    wait(lambda: command(['tmux', 'display-message', '-p', '-t', terminal.session,
         '#{pane_dead} #{pane_dead_status}']).strip() == '1 0', 'slash quit clean exit')
    terminal.drive('stop')
    terminal.session = None
    inputs = [v['input']['data']['text'] for k, v in committed(journey)
              if k == 'session.input.admitted.1']
    require(inputs[:3] == ['/not-a-command literal', 'Project a b.rs then second third',
                          'User branch-name'], 'literal or configured arguments changed')
    require(len(inputs) == 6 and inputs[-1] == EDIT, 'local command admitted extra model input')
    require('Input: \n' in inputs[3] and 'Input: feature-branch' in inputs[4],
            'review default or argument did not expand server template')


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    model, mail = CommandModel(), TuiMail()
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
            config['commands'] = {'project-check': {'template': 'Project $1 then $2'}}
            write(config_path, config)
            write(journey.workspace / 'grapt.jsonc', {'commands': config['commands']})
            user_commands = Path(journey.environment['GRAPT_CONFIG_DIR']) / 'commands'
            user_commands.mkdir()
            (user_commands / 'user-check.md').write_text('User $ARGUMENTS\n')
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
            result.update(passed=True, provider_requests=model.count())
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
