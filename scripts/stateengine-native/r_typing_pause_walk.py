#!/usr/bin/env python3
"""Exercise default/on/off typing pause with real input during a local stream.

This is functional mode coverage. Injected-time unit tests prove the deadline;
this walk makes no wall-clock latency or Windows performance claim.
"""
import argparse
import json
from pathlib import Path
import ssl
import subprocess
import sys
import threading
import time

from acceptance_peer import AGENT, CAPABILITY, PROJECT, Handler, Model, Server
from headless_acceptance import SESSION, require, write
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_walk import committed, idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]
DRAFT = 'UNSENT 中 draft'


class PacedWriter:
    """Reuse the ordinary HTTP fixture; pace only its bounded SSE body."""
    def __init__(self, writer, finished):
        self.writer, self.finished = writer, finished

    def __getattr__(self, name):
        return getattr(self.writer, name)

    def write(self, payload):
        if not payload.startswith(b'data: '):
            return self.writer.write(payload)
        for frame in payload.split(b'\n\n'):
            if frame:
                self.writer.write(frame + b'\n\n')
                self.writer.flush()
                time.sleep(.02)
        self.finished.set()
        return len(payload)


class PacedHandler(Handler):
    def setup(self):
        super().setup()
        self.wfile = PacedWriter(self.wfile, self.server.finished)


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary product required')
    model, mail, journey, terminal = Model(), TuiMail(), None, None
    def dispatch(path, headers, body):
        model.dispatch(path, headers, body)  # Existing route/binding/byte validation.
        events = [{'type': 'response.output_text.delta', 'delta': '```\n'}]
        events += [{'type': 'response.output_text.delta',
                    'delta': f'PAUSE_{model.count()}_CHUNK_{i:03}\n'}
                  for i in range(1, 201)]
        events += [{'type': 'response.output_text.delta', 'delta': '```\n'},
                   {'type': 'response.output_text.done'}, {'type': 'response.completed',
            'response': {'usage': {'input_tokens': 12, 'output_tokens': 200, 'total_tokens': 212}}}]
        return 'text/event-stream', ''.join('data: ' + json.dumps(event) + '\n\n' for event in events).encode()
    provider = Server(0, dispatch)
    provider.RequestHandlerClass = PacedHandler
    provider.finished = threading.Event()
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, source=args.source, ordinary_binaries=True, paid_calls=0,
                  boundary='Functional mode coverage; deadline checked by injected-time unit tests')
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            workspace = journey.root / 'workspace'
            workspace.mkdir()
            # Preparation creates a private repository boundary. Keep it with
            # the workspace so service configuration retains Profile origin.
            (journey.profile / '.git').rename(workspace / '.git')
            journey.start(0)
            identity = {'scope': 'project', 'project_id': PROJECT, 'agent_id': AGENT,
                        'mail_transport_protocol': 'remote-seat-mail-transport/v2'}
            provision = journey.cli(['session', 'provision', '--service', str(journey.profile),
                '--session', SESSION, '--agent', 'agt_live_headless1', '--model', 'openai/gpt-live',
                '--location', str(workspace), '--identity-entry', json.dumps(identity),
                '--packet-entry', json.dumps(journey.pointer)], 'provision', (CAPABILITY+'\n').encode())
            require(provision.returncode == 0, 'disposable provision failed')
            terminal = RTerminal(args, journey)
            modes = []
            for index, setting in enumerate((None, '50', '0')):
                provider.finished.clear()
                journey.environment.pop('GRAPT_R_TYPE_PAUSE_MS', None)
                if setting is not None:
                    journey.environment['GRAPT_R_TYPE_PAUSE_MS'] = setting
                terminal.start(index, directory=workspace)
                terminal.send('PAUSE_MODE_' + str(index))
                terminal.expect(f'PAUSE_{index + 1}_CHUNK_005')
                terminal.drive('send', '--text', '\x1b[200~' + DRAFT + 'X\x1b[201~')
                terminal.expect(DRAFT + 'X')
                require(not provider.finished.is_set(), 'fixture finished before input during streaming')
                terminal.capture('stream-input-' + str(index))
                terminal.drive('send', 'BSpace')
                terminal.expect(f'PAUSE_{index + 1}_CHUNK_200')
                idle(journey)
                screen = terminal.capture('completed-draft-' + str(index))
                require(DRAFT in screen and DRAFT + 'X' not in screen, 'streaming lost or changed input edit')
                require(model.count() == index + 1, 'draft edit submitted automatically')
                inputs = [row['input']['data']['text'] for kind, row in committed(journey)
                          if kind == 'session.input.admitted.1']
                require(inputs == [f'PAUSE_MODE_{i}' for i in range(index + 1)],
                        'unexpected canonical input admission')
                terminal.close()
                modes.append({'setting': setting, 'edited_draft_retained': True})
            require(not provider.errors and not peer.errors, 'fixture peer failure')
            result.update(passed=True, modes=modes, provider_requests=model.count())
        finally:
            if terminal:
                if terminal.session:
                    try: terminal.capture('failure')
                    except (OSError, RuntimeError): pass
                terminal.cleanup()
            if journey:
                write(journey.root / 'requests.json', model.bodies)
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
