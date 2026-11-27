#!/usr/bin/env python3
"""Real wheel CSI reports move R's transcript and preserve its composer draft.

Uses ordinary binaries, a disposable profile and isolated TLS/MCP peers.
"""
import argparse
import json
from pathlib import Path
import ssl
import subprocess
import sys

from acceptance_peer import Model, Server
from headless_acceptance import require, write
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_provider import text
from sustained_walk import committed, idle
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]


class WheelModel(Model):
    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        lines = '\n'.join(f'WHEEL_ROW_{i:03d}' for i in range(240))
        return text(f'```text\n{lines}\n```\nWHEEL_TAIL_DONE')


def exercise(terminal, journey, model):
    terminal.send('WHEEL_PROMPT_MUST_STAY_IN_HISTORY')
    terminal.expect('WHEEL_TAIL_DONE')
    idle(journey)
    terminal.capture('live-tail')
    terminal.drive('send', '--text', '\x1b[<64;4;10M' * 8)
    screen = terminal.until(
        lambda s: 'End live' in s and 'WHEEL_TAIL_DONE' not in s,
        'wheel-up moves transcript', seconds=8)
    require('WHEEL_PROMPT_MUST_STAY_IN_HISTORY' not in '\n'.join(screen.splitlines()[-7:]),
            'wheel recalled a prompt into composer')
    terminal.capture('wheel-up-empty-draft')
    terminal.drive('send', '--text', '\x1b[200~KEEP_WHEEL_DRAFT\x1b[201~')
    terminal.expect('KEEP_WHEEL_DRAFT')
    terminal.drive('send', '--text', '\x1b[<64;4;10M' * 3)
    terminal.expect('KEEP_WHEEL_DRAFT')
    terminal.capture('wheel-keeps-draft')
    terminal.drive('send', 'End')
    terminal.expect('WHEEL_TAIL_DONE')
    terminal.expect('KEEP_WHEEL_DRAFT')
    terminal.capture('end-live-keeps-draft')
    terminal.drive('send', 'PageUp')
    terminal.until(lambda s: 'WHEEL_TAIL_DONE' not in s, 'PageUp transcript')
    terminal.drive('send', '--text', '\x1b[<65;4;10M' * 40)
    terminal.expect('WHEEL_TAIL_DONE')
    terminal.expect('KEEP_WHEEL_DRAFT')
    require(model.count() == 1, 'wheel or End submitted a prompt')
    inputs = [v for k, v in committed(journey) if k == 'session.input.admitted.1']
    require(len(inputs) == 1, 'wheel admitted input')
    terminal.drive('send', 'C-a', 'C-k')
    terminal.drive('send', 'Up')
    terminal.until(lambda s: 'WHEEL_PROMPT_MUST_STAY_IN_HISTORY' in
                   '\n'.join(s.splitlines()[-8:]), 'keyboard Up still recalls the prompt')
    terminal.capture('keyboard-recall-still-works')
    terminal.drive('send', 'C-a', 'C-k')
    terminal.send('/help')
    terminal.expect('Shift+drag')
    terminal.capture('selection-help')


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source,
            'frontend source differs')
    backend = args.binaries / 'backend-source.txt'
    result = dict(passed=False, paid_calls=0, frontend_source=args.source,
                  backend_source=backend.read_text().strip() if backend.exists() else args.source)
    model, mail = WheelModel(), TuiMail()
    provider = Server(0, model.dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    journey, terminal, success = None, None, False
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            journey.start(0)
            require(journey.provision().returncode == 0, 'provision failed')
            terminal = RTerminal(args, journey)
            terminal.start(0)
            exercise(terminal, journey, model)
            require(not provider.errors and not peer.errors, 'fixture peer failed')
            terminal.close()
            journey.stop()
            success = True
        finally:
            errors = []
            if terminal:
                if terminal.session:
                    try:
                        terminal.capture('failure')
                    except (OSError, RuntimeError):
                        pass
                try:
                    terminal.cleanup()
                except (OSError, RuntimeError) as error:
                    errors.append(f'terminal cleanup: {error}')
            if journey:
                try:
                    journey.cleanup()
                except (OSError, RuntimeError) as error:
                    errors.append(f'profile cleanup: {error}')
            result.update(passed=success and not errors, cleanup_errors=errors,
                          real_sgr_wheel=True, draft_preserved=success, no_recall=success,
                          page_keys=success, end_live=success, native_selection_help=success)
            write(args.scratch / 'result.json', result)
            if errors:
                raise RuntimeError('; '.join(errors))
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
    args.binaries = args.binaries.resolve(strict=True)
    args.scratch = args.scratch.resolve()
    require(args.scratch != ROOT and ROOT not in args.scratch.parents, 'scratch must be outside source')
    args.scratch.mkdir(mode=0o700)
    subprocess.run([sys.executable, str(ROOT / 'scripts/native-tui/fixture-ca.py'),
                    '--root', str(args.scratch / 'tls')], check=True, timeout=30)
    subprocess.run(['bwrap', '--die-with-parent', '--new-session', '--unshare-pid', '--unshare-net',
        '--ro-bind', '/', '/', '--tmpfs', '/home', '--tmpfs', '/tmp', '--tmpfs', '/var/tmp',
        '--proc', '/proc', '--dev', '/dev', '--bind', str(args.scratch), str(args.scratch),
        '--chdir', str(args.scratch), '/usr/bin/env', '-i', 'PATH=/usr/bin:/bin',
        'PYTHONDONTWRITEBYTECODE=1', f'HOME={args.scratch}/empty-home', sys.executable,
        str(Path(__file__)), '--inside', '--binaries', str(args.binaries), '--source', args.source,
        '--scratch', str(args.scratch)], check=True, timeout=180)


if __name__ == '__main__':
    main()
