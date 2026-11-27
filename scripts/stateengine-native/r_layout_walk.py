#!/usr/bin/env python3
"""Check real R header/footer geometry in disposable Linux PTYs at four widths."""
import argparse
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys

from acceptance_peer import AGENT, CAPABILITY, PROJECT, Model, Server
from headless_acceptance import SESSION, require, write
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_walk import idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]
TITLE = 'Long session title ' * 4 + 'TITLE_END'
HISTORY = '```\n' + '\n'.join(
    f'LAYOUT_LINE_{index:03} stable transcript content' for index in range(1, 61)) + '\n```\n'
HINTS = {'/help', 'Connected', 'enter send', 'Esc sessions', '↑↓/ctrl+r recall',
         'ctrl+p sessions', 'F3 model', 'F7 agent', 'F6 questions (0)', 'pgup history', 'F2 theme'}


class LayoutTerminal(RTerminal):
    def start(self, width, prompt=None, directory=None):
        self.session = command([self.args.driver, 'start', '--seat', 'seat-a', '--journey',
            f'r-layout-{os.getpid()}-{width}', '--width', str(width), '--height', '30',
            '--cwd', self.journey.profile, '--', '/usr/bin/env', '-i',
            *[f'{key}={value}' for key, value in self.journey.environment.items()],
            self.args.tui, '--service', self.journey.profile, '--session', SESSION,
            '--directory', self.journey.workspace], self.journey.environment).strip()
        self.expect('Connected')
        self.expect('gpt-live')

    def rename(self, title):
        self.drive('send', '--text', '\x1b[200~/rename ' + title + '\x1b[201~')
        self.expect(title.split()[-1])
        self.drive('send', 'Enter')
        self.expect('Session renamed.')
        require(self.journey.api('GET', '/api/session/' + SESSION)['data']['title'] == title,
                'rename did not retain the exact title')


def geometry(screen, width, seat_title=False):
    rows = screen.splitlines()
    require(len(rows) == 30, f'{width}: footer wrapped outside the frame')
    require(all(len(row) <= width for row in rows), f'{width}: row exceeded terminal width')
    rows = [row.ljust(width) for row in rows]
    chip = rows[0].index('F3')
    require(rows[0][chip - 1] == ' ' and 'gpt-live' in rows[0], f'{width}: header collision')
    require(('Ada · seat' in rows[0]) if seat_title else ('…' in rows[0]),
            f'{width}: title fallback/truncation absent')
    require(rows[1].startswith(' Cache last'), f'{width}: cache row is not inset')
    for y in (0, 1, 28, 29):
        require(rows[y][0] == ' ' and rows[y][-1] == ' ', f'{width}: edge content on row {y}')
    borders = [row for row in rows if row.startswith(('┌', '└'))]
    require(len(borders) == 2, f'{width}: composer frame missing')
    require(all(not any(value in row for value in ('Cache last', '/help', 'Transcript'))
                for row in borders), f'{width}: labels overlap composer border')
    require(rows[28].strip().startswith('/help'), f'{width}: footer row missing')
    # A command notice can replace Connected, but static hints must stay whole.
    hints = rows[28].strip().split(' · ')
    require(all(hint in HINTS or hint == 'Session renamed.' for hint in hints),
            f'{width}: clipped or wrapped footer hint: {hints}')
    require('Transcript' in rows[29], f'{width}: history position left the footer')


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary product required')
    model, mail, journey, terminal = Model(), TuiMail(), None, None
    def dispatch(path, headers, body):
        kind, payload = model.dispatch(path, headers, body)
        return kind, payload.replace(b'ROW_A_PACKET_DONE', json.dumps(HISTORY)[1:-1].encode())
    provider = Server(0, dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, source=args.source, ordinary_binaries=True, paid_calls=0)
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            journey.workspace = journey.root / 'workspace'
            journey.workspace.mkdir()
            # Keep executable service configuration outside the fixture repo.
            (journey.profile / '.git').rename(journey.workspace / '.git')
            journey.start(0)
            identity = {'scope': 'project', 'project_id': PROJECT, 'agent_id': AGENT,
                        'mail_transport_protocol': 'remote-seat-mail-transport/v2'}
            provision = journey.cli(['session', 'provision', '--service', str(journey.profile),
                '--session', SESSION, '--agent', 'agt_live_headless1', '--model', 'openai/gpt-live',
                '--location', str(journey.workspace), '--identity-entry', json.dumps(identity),
                '--packet-entry', json.dumps(journey.pointer)], 'provision', (CAPABILITY+'\n').encode())
            require(provision.returncode == 0, 'disposable provision failed')
            terminal = LayoutTerminal(args, journey)
            terminal.start(120)
            terminal.send('LAYOUT_HISTORY_FIXTURE')
            terminal.expect('LAYOUT_LINE_060')
            idle(journey)
            terminal.rename(TITLE)
            terminal.close()
            for width in (80, 100, 120, 107):
                terminal.start(width)
                terminal.expect('LAYOUT_LINE_060')
                geometry(terminal.capture(f'layout-{width}'), width)
                if width == 107:
                    terminal.rename('[seat-runner] You are Ada. Seat serial: agt_layout TITLE_END')
                    geometry(terminal.capture('layout-107-seat-title'), width, seat_title=True)
                terminal.close()
            require(model.count() == 1, 'local layout operations invoked the provider')
            require(not provider.errors and not peer.errors, 'fixture peer failure')
            result.update(passed=True, widths=[80, 100, 120, 107], provider_requests=model.count(),
                          whole_footer_hints=True, title_gap=True, inset_status=True)
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
