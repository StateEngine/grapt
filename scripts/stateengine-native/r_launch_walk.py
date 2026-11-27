#!/usr/bin/env python3
"""Actual R --prompt launch with ordinary Product/MCP and isolated PTY faults."""
import argparse
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys

from acceptance_peer import Model, Server
from headless_acceptance import SESSION, require, wait, write
from launch_proxy import Proxy
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_walk import committed, idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]
TEXT = '/literal startup prompt\nsecond line\tvalue'


class Terminal(RTerminal):
    def start(self, index, text=TEXT):
        self.session = command([self.args.driver, 'start', '--seat', 'seat-a', '--journey',
            f'r-launch-{os.getpid()}-{index}', '--width', '160', '--height', '45',
            '--cwd', self.journey.profile, '--', '/usr/bin/env', '-i',
            *[f'{key}={value}' for key, value in self.journey.environment.items()],
            self.args.tui, '--service', self.journey.profile, '--session', SESSION,
            *(['--prompt', text] if text is not None else [])], self.journey.environment).strip()
        self.expect('enter send')


def admitted(journey):
    return [value for kind, value in committed(journey) if kind == 'session.input.admitted.1']


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary product required')
    model, mail, journey, terminal = Model(), TuiMail(), None, None
    provider = Server(0, model.dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, source=args.source, ordinary_binaries=True, paid_calls=0)
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            journey.start(0)
            require(journey.provision().returncode == 0, 'disposable provision failed')
            terminal = Terminal(args, journey)
            receipt = journey.profile / 'serve-ready.json'
            readiness = json.loads(receipt.read_text())
            with Proxy(journey.endpoint) as proxy:
                write(receipt, dict(readiness, observed_endpoint=proxy.endpoint))
                proxy.hold()
                terminal.start(0)
                wait(proxy.entered.is_set, 'delayed exact-session metadata')
                require('/literal startup prompt' not in terminal.capture('waiting-visible'), 'startup packet entered human composer')
                require(not admitted(journey), 'prompt sent before choices')
                proxy.release.set()
                terminal.expect('ROW_A_PACKET_DONE')
                idle(journey)
                require(len(admitted(journey)) == 1 and model.count() == 1, 'automatic prompt not exactly once')
                terminal.close()

                proxy.hold()
                terminal.start(1, 'edited launch')
                wait(proxy.entered.is_set, 'editable startup read')
                terminal.drive('send', '--text', 'X')
                terminal.expect('X')
                proxy.release.set()
                terminal.expect('Connected')
                require(len(admitted(journey)) == 1, 'edited launch sent automatically')
                terminal.drive('send', 'Enter')
                wait(lambda: len(admitted(journey)) == 2, 'explicit edited admission')
                idle(journey)
                terminal.close()

                proxy.hold()
                terminal.start(2, 'navigation cancels')
                wait(proxy.entered.is_set, 'navigation startup read')
                terminal.drive('send', 'Escape')
                terminal.expect('Sessions')
                proxy.release.set()
                terminal.expect('Connected')
                require(len(admitted(journey)) == 2, 'navigation sent startup text')
                terminal.close()

                for index, prefix, suffix in [(3, '\x1b[200~unfinished paste', '\x1b[201~'),
                                               (4, '\x1b[', 'D')]:
                    proxy.hold()
                    terminal.start(index, 'split input launch')
                    wait(proxy.entered.is_set, 'split-input startup read')
                    terminal.drive('send', '--text', prefix)
                    proxy.release.set()
                    terminal.expect('Connected')
                    require(len(admitted(journey)) == 2, 'unfinished physical input lost cancellation')
                    terminal.drive('send', '--text', suffix)
                    require('split input launch' not in terminal.capture('split-input-clean'), 'canceled packet entered composer')
                    require(len(admitted(journey)) == 2, 'split input submitted automatically')
                    terminal.capture('split-input-' + str(index))
                    terminal.close()

                proxy.fail = True
                terminal.start(5, 'retry with pending paste')
                terminal.expect('Ctrl+O retry reads')
                proxy.hold()
                terminal.drive('send', '--text', '\x0f\x1b[200~unfinished retry')
                wait(proxy.entered.is_set, 'retry with incomplete paste')
                proxy.release.set()
                terminal.expect('Connected')
                require(len(admitted(journey)) == 2, 'read retry bypassed unfinished paste')
                terminal.drive('send', '--text', '\x1b[201~')
                terminal.expect('unfinished retry')
                require('retry with pending paste' not in terminal.capture('retry-paste-clean'), 'packet entered edited draft')
                require(len(admitted(journey)) == 2, 'retry paste submitted automatically')
                terminal.close()

                # Exercise the established editor keys both before startup is
                # ready and on an ordinary attach with no --prompt at all.
                for index, text in [(8, 'control prefix delete me'), (9, None)]:
                    proxy.hold()
                    terminal.start(index, text)
                    wait(proxy.entered.is_set, 'editor-control attach read')
                    if text is None:
                        proxy.release.set()
                        terminal.expect('Connected')
                    terminal.drive('send', '--text', 'control prefix delete me')
                    terminal.expect('control prefix delete me')
                    terminal.drive('send', '--text', '\n')  # Ctrl+J deletes to line start.
                    terminal.drive('send', '--text', 'held key delete')
                    terminal.expect('held key delete')
                    terminal.drive('send', '--text', '\x1bh')  # Alt+H deletes the previous word.
                    terminal.drive('send', '--text', 'kept')
                    screen = terminal.expect('held key kept')
                    require('Sessions' not in screen and 'control prefix delete me' not in screen,
                            'control/Alt edit navigated or failed to delete')
                    proxy.release.set()
                    terminal.expect('Connected')
                    require(len(admitted(journey)) == 2, 'editor-control key submitted a prompt')
                    terminal.capture('editor-controls-' + str(index))
                    terminal.close()

                proxy.fail = True
                terminal.start(6, 'read retry')
                terminal.expect('Ctrl+O retry reads')
                require(len(admitted(journey)) == 2, 'failed read submitted')
                terminal.drive('send', 'C-o')
                wait(lambda: len(admitted(journey)) == 3, 'explicit read retry')
                idle(journey)
                terminal.close()

                proxy.lose_write, proxy.hide_history = True, True
                terminal.start(7, 'uncertain startup')
                terminal.expect('Alt+R')
                wait(lambda: len(admitted(journey)) == 4, 'uncertain original write')
                idle(journey)
                require('uncertain startup' not in terminal.capture('uncertain-draft'), 'uncertain machine input entered composer')
                terminal.drive('send', 'F2')
                require('uncertain startup' not in terminal.capture('uncertain-still-clean'), 'read retry exposed machine input')
                require(len(admitted(journey)) == 4, 'uncertain startup replayed')
                terminal.close()
                writes = [(method, path) for method, path in proxy.requests if method != 'GET']
                write(journey.root / 'proxy-writes.json', writes)
                require(len(writes) == 4 and all(path.endswith('/prompt') for _, path in writes), 'unexpected launch write')
                require(not proxy.errors, 'proxy failure: ' + str(proxy.errors))
            rows = admitted(journey)
            require([row['input']['data']['text'] for row in rows] ==
                    [TEXT, 'X', 'read retry', 'uncertain startup'], 'literal startup text changed')
            require(len({row['inputID'] for row in rows}) == 4 and all(row['sessionID'] == SESSION for row in rows), 'input identity changed')
            require(not provider.errors and not peer.errors and model.count() == 4, 'provider/MCP journey failed')
            calls = (journey.root / 'mcp-calls.txt').read_text()
            require('stateengine_resume' in calls, 'MCP bootstrap missing')
            result.update(passed=True, delayed_choices=True, edit_and_navigation_cancel=True,
                explicit_read_retry=True, split_paste_and_escape_cancel=True,
                raw_editor_controls=True, uncertain_write_no_retry=True, exact_admissions=4)
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
                write(journey.root / 'model-requests.json', model.bodies)
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
        '--scratch', str(args.scratch)], check=True, timeout=240)


if __name__ == '__main__':
    main()
