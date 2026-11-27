#!/usr/bin/env python3
"""An ordinary execution failure restores its exact input; Enter resends once."""
import argparse
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys

from acceptance_peer import Model, Server
from headless_acceptance import require, wait, write
from ordinary_profile import OrdinaryProfile
from r_launch_walk import admitted
from r_sustained_terminal import RTerminal
from sustained_walk import idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]
TEXT = '  failed input 中👩🏽‍💻\nsecond line\n'
FAILURE = 'INJECTED_REQUEST_FAILURE'


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary product required')
    model, mail, journey, terminal = Model(), TuiMail(), None, None
    attempts = []
    def dispatch(path, headers, body):
        attempts.append(body)
        require(len(attempts) <= args.prior_successes + 2, 'unexpected automatic retry or duplicate Enter submission')
        if len(attempts) == args.prior_successes + 1:
            raise ValueError(FAILURE)  # Handler returns terminal HTTP 400, not a retryable status.
        kind, payload = model.dispatch(path, headers, body)
        if len(attempts) <= args.prior_successes:
            payload = payload.replace(b'ROW_A_PACKET_DONE', f'PRIOR_{len(attempts)}_DONE'.encode())
        return kind, payload
    provider = Server(0, dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, source=args.source, ordinary_binaries=True, paid_calls=0)
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            journey.start(0)
            require(journey.provision().returncode == 0, 'disposable provision failed')
            terminal = RTerminal(args, journey)
            terminal.start(0)
            for index in range(args.prior_successes):
                terminal.drive('send', '--text', f'previous successful prompt {index + 1}')
                terminal.drive('send', 'Enter')
                terminal.expect(f'PRIOR_{index + 1}_DONE')
                idle(journey)
            terminal.drive('send', '--text', '\x1b[200~' + TEXT + '\x1b[201~')
            terminal.drive('send', 'Enter')
            terminal.expect('Execution failed · draft kept')
            idle(journey)
            require(len(attempts) == args.prior_successes + 1 and len(admitted(journey)) == args.prior_successes + 1, 'failed request replayed')
            screen = terminal.capture('failed-input-restored')
            require('failed input 中👩🏽‍💻' in screen and 'second line' in screen, 'failed input not editable')
            terminal.drive('send', 'Enter')
            terminal.expect('ROW_A_PACKET_DONE')
            idle(journey)
            rows = admitted(journey)
            require(len(rows) == len(attempts) == args.prior_successes + 2, 'explicit resend was not exactly once')
            require([row['input']['data']['text'] for row in rows[-2:]] == [TEXT, TEXT], 'resend changed exact input')
            terminal.capture('explicit-resend-completed')
            terminal.close()
            require(provider.errors == [FAILURE] and not peer.errors, 'unexpected fixture error')
            result.update(passed=True, prior_successes=args.prior_successes,
                          exact_failed_input_restored=True, exact_explicit_resend=True,
                          admissions=len(rows), provider_requests=len(attempts))
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
    parser.add_argument('--prior-successes', type=int, choices=(0, 2), default=0)
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
        '--scratch', str(args.scratch), '--prior-successes', str(args.prior_successes)], check=True, timeout=180)


if __name__ == '__main__':
    main()
