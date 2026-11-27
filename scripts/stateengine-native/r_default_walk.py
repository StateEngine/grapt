#!/usr/bin/env python3
"""Plain grapt opens R; its existing session and file operations remain usable."""
import argparse
import json
import os
from pathlib import Path
import shutil
import sqlite3
import ssl
import subprocess
import sys

from acceptance_peer import Model, Server
from headless_acceptance import require, wait, write
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_provider import calls, text
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]
PROMPT = 'R_SHIP_PATCH: add rship.txt in this disposable repository.'

class PatchModel(Model):
    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        if self.count() == 1:
            return calls([('call_rship_patch', 'apply_patch',
                '*** Begin Patch\n*** Add File: rship.txt\n+R works through the normal product tool\n*** End Patch')])
        require(self.count() == 2 and 'call_rship_patch' in json.dumps(body), 'unexpected provider request')
        return text('R_SHIP_PATCH_DONE')

class Terminal(RTerminal):
    def start(self, index, plain=False):
        self.session = command([self.args.driver, 'start', '--seat', 'seat-a', '--journey',
            f'r-default-{os.getpid()}-{index}', '--width', '160', '--height', '45',
            '--cwd', self.journey.repository, '--', '/usr/bin/env', '-i',
            *[f'{key}={value}' for key, value in self.journey.environment.items()],
            self.args.cli, *([] if plain else ['code'])], self.journey.environment).strip()
        self.expect('enter send')

    def close_after_quit(self):
        def exited():
            state = command(['tmux', 'display-message', '-p', '-t', self.session,
                             '#{pane_dead} #{pane_dead_status}']).strip()
            require(not state.startswith('1 ') or state == '1 0', 'CLI/R quit failed')
            return state == '1 0'
        wait(exited, 'CLI and R clean quit', seconds=30)
        self.drive('stop')
        self.session = None


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary product required')
    model, mail, journey, terminal = PatchModel(), TuiMail(), None, None
    provider = Server(0, model.dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, source=args.source, ordinary_binaries=True, paid_calls=0)
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            # The normal no-argument CLI discovers this real repository profile.
            repository = journey.root / 'repo'
            repository.mkdir()
            subprocess.run(['git', 'init', '--quiet', str(repository)], check=True, timeout=10)
            # Exact checkpoint capture deliberately refuses an unborn Git root.
            (repository / 'README.md').write_text('Disposable R shipping fixture.\n')
            subprocess.run(['git', '-C', str(repository), 'add', '--', 'README.md'], check=True, timeout=10)
            subprocess.run(['git', '-C', str(repository), '-c', 'user.name=R fixture',
                '-c', 'user.email=r-fixture@example.invalid', '-c', 'commit.gpgsign=false',
                'commit', '--quiet', '-m', 'Fixture baseline', '--', 'README.md'], check=True, timeout=10)
            # Keep the normal user profile outside the workspace so file undo
            # can record its restore journal without recursively snapshotting it.
            configuration = journey.root / 'user-config'
            profile = configuration / 'grapt/default'
            profile.parent.mkdir(parents=True)
            shutil.move(str(journey.profile), profile)
            journey.profile = profile
            journey.repository = repository
            journey.environment['XDG_CONFIG_HOME'] = str(configuration)
            journey.start(0)
            terminal = Terminal(args, journey)
            terminal.start(0, plain=True)
            terminal.expect('Agents · Enter select')
            terminal.drive('send', 'Enter')
            terminal.expect('Models and effort')
            terminal.drive('send', 'Enter')
            terminal.expect('Connected')
            require(journey.api('GET', '/api/session')['data'] == [], 'opening pickers created a session')
            terminal.send(PROMPT)
            terminal.until(lambda screen: 'R_SHIP_PATCH_DONE' in screen, 'real patch completion')
            target = repository / 'rship.txt'
            require(target.read_text() == 'R works through the normal product tool\n', 'actual patch effect missing')
            sessions = journey.api('GET', '/api/session')['data']
            require(len(sessions) == 1, 'new draft created duplicate sessions')
            selected = sessions[0]['id']
            terminal.send('/rename R shipping session')
            terminal.expect('R shipping session')
            terminal.send('/undo')
            terminal.expect('FILE UNDO')
            require(target.is_file(), 'undo preview changed files')
            terminal.drive('send', 'Escape')
            terminal.send('/undo apply')
            terminal.expect('FILE UNDO')
            wait(lambda: not target.exists(), 'reviewed file undo')
            terminal.drive('send', 'Escape')
            terminal.send('/rewind')
            terminal.expect('Conversation rewound; files unchanged.')
            terminal.expect(PROMPT)
            require(model.count() == 2, 'slash command submitted a provider request')
            # Discard the recalled single-line input before the explicit quit.
            terminal.drive('send', '--text', '\x15')
            terminal.send('/quit')
            terminal.close_after_quit()
            require(journey.child.poll() is None, 'attached CLI stopped a product it did not own')
            journey.stop()
            terminal.start(1)
            terminal.expect('Agents · Enter select')
            journey.endpoint = json.loads((journey.profile / 'serve-ready.json').read_text())['observed_endpoint']
            terminal.drive('send', 'Escape')
            wait(lambda: 'Agents · Enter select' not in terminal.drive('capture'), 'closed agent picker')
            terminal.drive('send', '--text', '\x10')
            terminal.expect('Sessions')
            terminal.expect('R shipping session')
            terminal.drive('send', 'Enter')
            terminal.expect('R shipping session')
            terminal.capture('resumed-through-code')
            require(journey.api('GET', f'/api/session/{selected}')['data']['id'] == selected, 'saved session identity changed')
            terminal.send('/quit')
            terminal.close_after_quit()
            require(not provider.errors and not peer.errors and model.count() == 2, 'provider/MCP journey failed')
            result.update(passed=True, plain_default_r=True, code_resume=True, explicit_new_session=True,
                rename=True, reviewed_file_undo=True, rewind_for_editing=True, slash_quit=True,
                attached_product_kept=True, owned_product_drained=True)
        finally:
            if terminal:
                if terminal.session:
                    try: terminal.capture('failure')
                    except (OSError, RuntimeError): pass
                terminal.cleanup()
            if journey:
                if journey.child and journey.child.poll() is None:
                    try: journey.stop()
                    except (OSError, RuntimeError, subprocess.TimeoutExpired): pass
                journey.cleanup()
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
    subprocess.run([sys.executable, str(ROOT / 'scripts/native-tui/fixture-ca.py'), '--root', str(args.scratch / 'tls')], check=True, timeout=30)
    subprocess.run(['bwrap', '--die-with-parent', '--new-session', '--unshare-pid', '--unshare-net',
        '--ro-bind', '/', '/', '--tmpfs', '/home', '--tmpfs', '/tmp', '--tmpfs', '/var/tmp',
        '--proc', '/proc', '--dev', '/dev', '--bind', str(args.scratch), str(args.scratch),
        '--ro-bind', str(ROOT), str(ROOT), '--ro-bind', str(args.binaries), str(args.binaries),
        '--chdir', str(args.scratch), '/usr/bin/env', '-i', 'PATH=/usr/bin:/bin',
        'PYTHONDONTWRITEBYTECODE=1', f'HOME={args.scratch}/empty-home', sys.executable,
        str(Path(__file__)), '--inside', '--binaries', str(args.binaries), '--source', args.source,
        '--scratch', str(args.scratch)], check=True, timeout=180)


if __name__ == '__main__':
    main()
