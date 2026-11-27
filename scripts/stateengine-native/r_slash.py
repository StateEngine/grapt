#!/usr/bin/env python3
"""Ordinary R workspace command completion, review and quoted argument PTY journey."""
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
CUSTOM = 'SLASH_CONFIG a b.rs then 界.rs'


class CommandModel(Model):
    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        expected = 'Input: \n\n' if self.count() == 1 else CUSTOM
        require(json.dumps(expected)[1:-1] in json.dumps(body),
                'expanded workspace command absent from provider request')
        return text('SLASH_REVIEW_DONE' if self.count() == 1 else 'SLASH_CUSTOM_DONE')


def exercise(terminal, journey, model):
    terminal.drive('send', '--text', '\x1b[200~/rev\x1b[201~')
    terminal.expect('/ commands')
    terminal.expect('/review')
    terminal.drive('send', 'Tab')
    terminal.until(lambda s: '/review' in '\n'.join(s.splitlines()[-7:]), 'completed review draft')
    require(model.count() == 0, 'completion submitted a prompt')
    require(not any(k == 'session.input.admitted.1' for k, _ in committed(journey)),
            'completion admitted input')
    terminal.drive('send', 'Enter')
    terminal.expect('SLASH_REVIEW_DONE')
    idle(journey)
    terminal.drive('send', '--text', '\x1b[200~/fo\x1b[201~')
    terminal.expect('/focus')
    terminal.drive('send', 'Tab')
    terminal.drive('send', '--text', '\x1b[200~"a b.rs" \'界.rs\'\x1b[201~')
    terminal.capture('quoted-command-arguments')
    require(model.count() == 1, 'custom completion submitted input')
    terminal.drive('send', 'Enter')
    terminal.expect('SLASH_CUSTOM_DONE')
    idle(journey)
    terminal.drive('send', '--text', '\x1b[200~/focus one\x1b[201~')
    terminal.drive('send', 'Enter')
    terminal.expect('Command needs argument $2')
    terminal.capture('missing-argument-keeps-draft')
    inputs = [v['input']['data']['text'] for k, v in committed(journey)
              if k == 'session.input.admitted.1']
    require(len(inputs) == 2 and 'Input: \n\n' in inputs[0] and inputs[1] == CUSTOM,
            'unexpected command prompt admission')
    require(model.count() == 2, 'unexpected provider call')


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source,
            'frontend source differs')
    backend = args.binaries / 'backend-source.txt'
    result = dict(passed=False, paid_calls=0, frontend_source=args.source,
                  backend_source=backend.read_text().strip() if backend.exists() else args.source)
    model, mail = CommandModel(), TuiMail()
    provider = Server(0, model.dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    journey, terminal, success = None, None, False
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            subprocess.run(['git', 'init', '--quiet', str(journey.profile)],
                           check=True, timeout=15)
            directory = journey.profile / '.grapt/commands'
            directory.mkdir(parents=True)
            (directory / 'focus.md').write_text(
                '---\ndescription: Focus on two files\n---\nSLASH_CONFIG $1 then $2\n')
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
            result.update(passed=success and not errors, cleanup_errors=errors)
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
