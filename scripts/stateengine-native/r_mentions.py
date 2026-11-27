#!/usr/bin/env python3
"""Ordinary R file search, path insertion and Escape-to-literal PTY journey."""
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
REFERENCE = 'read "src/mention-proof space.md" '
LITERAL = 'check @mention-proof'


class MentionModel(Model):
    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        expected = REFERENCE if self.count() == 1 else LITERAL
        require(json.dumps(expected)[1:-1] in json.dumps(body),
                'completed or literal reference absent from request')
        return text('MENTION_PATH_DONE' if self.count() == 1 else 'MENTION_LITERAL_DONE')


def exercise(terminal, journey, model):
    terminal.drive('send', '--text', '\x1b[200~read @mention-proof\x1b[201~')
    terminal.expect('@ workspace files')
    terminal.expect('src/mention-proof space.md')
    screen = terminal.capture('workspace-matches')
    require('mention-proof-hidden.md' not in screen, 'ignored file offered')
    terminal.drive('send', 'Tab')
    terminal.until(lambda s: '@ workspace files' not in s and REFERENCE.strip() in
                   '\n'.join(s.splitlines()[-7:]), 'selected path in composer')
    terminal.capture('quoted-path-in-draft')
    require(model.count() == 0, 'file selection submitted a prompt')
    require(not any(k == 'session.input.admitted.1' for k, _ in committed(journey)),
            'file selection admitted input')
    terminal.drive('send', 'Enter')
    terminal.expect('MENTION_PATH_DONE')
    idle(journey)
    terminal.drive('send', '--text', '\x1b[200~' + LITERAL + '\x1b[201~')
    terminal.expect('@ workspace files')
    terminal.drive('send', 'Escape')
    terminal.until(lambda s: '@ workspace files' not in s and LITERAL in s,
                   'Escape preserves literal query')
    terminal.capture('literal-reference')
    terminal.drive('send', 'Enter')
    terminal.expect('MENTION_LITERAL_DONE')
    idle(journey)
    inputs = [v['input']['data']['text'] for k, v in committed(journey)
              if k == 'session.input.admitted.1']
    require(inputs == [REFERENCE, LITERAL], 'unexpected prompt admission')
    require(model.count() == 2, 'unexpected provider call')


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source,
            'frontend source differs')
    backend = args.binaries / 'backend-source.txt'
    result = dict(passed=False, paid_calls=0, frontend_source=args.source,
                  backend_source=backend.read_text().strip() if backend.exists() else args.source)
    model, mail = MentionModel(), TuiMail()
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
            (journey.profile / 'src').mkdir()
            (journey.profile / 'src/mention-proof space.md').write_text('mention fixture\n')
            (journey.profile / 'mention-proof-hidden.md').write_text('ignored fixture\n')
            (journey.profile / '.gitignore').write_text('mention-proof-hidden.md\n')
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
