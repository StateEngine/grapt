#!/usr/bin/env python3
"""Ordinary isolated PTY: pre-provider failure preserves human input provenance."""
import argparse
import json
from pathlib import Path
import ssl
import subprocess
import sys

SOURCE = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(SOURCE / 'scripts/stateengine-native'))
from acceptance_peer import Model, Server
from headless_acceptance import require, wait, write
from ordinary_profile import OrdinaryProfile
from r_launch_walk import Terminal, admitted
from sustained_walk import idle
from tui_acceptance_peer import TuiMail

PACKET = '[seat-runner] MACHINE_STARTUP_SENTINEL\nCheck admitted scratch work. MACHINE_PACKET_END'
HUMAN = '  human failed draft 中\nsecond line\n'


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'wrong binary source')
    model, mail = Model(), TuiMail()
    provider = Server(0, model.dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    journey = terminal = None
    result = dict(passed=False, case=args.case, source=args.source, paid_calls=0)
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            journey.start(0)
            require(journey.provision().returncode == 0, 'disposable provision failed')
            journey.packet.unlink()  # No successful bootstrap can cache this packet.
            terminal = Terminal(args, journey)
            terminal.start(0, PACKET if args.case == 'startup' else None)
            if args.case == 'human':
                terminal.expect('Connected')
                terminal.drive('send', '--text', '\x1b[200~' + HUMAN + '\x1b[201~')
                terminal.drive('send', 'Enter')
                terminal.expect('Execution failed · draft kept')
                idle(journey)
                terminal.drive('send', 'Enter')
                wait(lambda: len(admitted(journey)) == 2, 'explicit human resend')
                terminal.expect('Execution failed · draft kept')
                idle(journey)
                rows = admitted(journey)
                require([row['input']['data']['text'] for row in rows] == [HUMAN, HUMAN], 'human resend changed text')
                terminal.capture('human-restored-and-resent')
            else:
                if args.case == 'mail':
                    terminal.expect('Connected')
                    mail.publish('msg_tui_idle')
                terminal.expect('Wake failed')
                screen = terminal.capture('automated-collapsed-before-check')
                idle(journey)
                rows = admitted(journey)
                require(len(rows) == 1, 'automated input admission count differs')
                label = 'Seat startup prompt' if args.case == 'startup' else 'Automated prompt'
                require(label in screen and 'F8 expand' in screen, 'no compact automated summary')
                require('draft kept' not in screen, 'automated failure claimed human draft recovery')
                require('MACHINE_STARTUP_SENTINEL' not in screen and 'msg_tui_idle' not in screen,
                        'collapsed transcript exposed machine payload')
                terminal.capture('automated-collapsed')
                terminal.drive('send', 'F8')
                expanded = terminal.expect('F8 collapse')
                require(('MACHINE_STARTUP_SENTINEL' if args.case == 'startup' else 'msg_tui_idle') in expanded,
                        'expanded transcript lost exact automated input')
                terminal.capture('automated-expanded')
                terminal.drive('send', 'F8')
                terminal.expect('F8 expand')
                terminal.drive('send', 'Up')
                terminal.drive('send', 'Enter')
                # A completed capture synchronizes a redraw after the explicit keys.
                terminal.capture('automated-not-recalled')
                idle(journey)
                require(len(admitted(journey)) == 1, 'automated input entered human recall/composer')
                terminal.drive('send', '--text', '\x1b[200~HUMAN_AFTER_WAKE\x1b[201~')
                terminal.drive('send', 'Enter')
                terminal.expect('Execution failed · draft kept')
                idle(journey)
                require(admitted(journey)[-1]['input']['data']['text'] == 'HUMAN_AFTER_WAKE',
                        'automated failure contaminated the human composer')
            require(model.count() == 0 and not provider.errors and not peer.errors,
                    'pre-provider refusal reached model or fixture failed')
            require('launch packet is absent' in json.dumps(journey.history()), 'wrong pre-provider failure cause')
            terminal.close()
            result.update(passed=True, provider_requests=0, admissions=len(admitted(journey)))
        finally:
            if terminal:
                if terminal.session:
                    try: terminal.capture('failure')
                    except (OSError, RuntimeError): pass
                terminal.cleanup()
            if journey:
                if journey.child and journey.child.poll() is None: journey.stop()
                journey.cleanup()
            write(args.scratch / 'result.json', result)
    print(json.dumps(result), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binaries', type=Path, required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--case', choices=('startup', 'mail', 'human'), required=True)
    parser.add_argument('--inside', action='store_true')
    args = parser.parse_args()
    if args.inside: return run(args)
    args.binaries, args.scratch = args.binaries.resolve(strict=True), args.scratch.resolve()
    require(args.scratch != SOURCE and SOURCE not in args.scratch.parents, 'private scratch required')
    args.scratch.mkdir(mode=0o700)
    subprocess.run([sys.executable, str(SOURCE / 'scripts/native-tui/fixture-ca.py'), '--root', str(args.scratch / 'tls')], check=True, timeout=30)
    subprocess.run(['bwrap', '--die-with-parent', '--new-session', '--unshare-pid', '--unshare-net',
        '--ro-bind', '/', '/', '--tmpfs', '/home', '--tmpfs', '/tmp', '--tmpfs', '/var/tmp',
        '--proc', '/proc', '--dev', '/dev', '--bind', str(args.scratch), str(args.scratch),
        '--chdir', str(args.scratch), '/usr/bin/env', '-i', 'PATH=/usr/bin:/bin',
        'PYTHONDONTWRITEBYTECODE=1', f'HOME={args.scratch}/empty-home', sys.executable,
        str(Path(__file__)), '--inside', '--binaries', str(args.binaries), '--source', args.source,
        '--scratch', str(args.scratch), '--case', args.case], check=True, timeout=180)

if __name__ == '__main__': main()
