#!/usr/bin/env python3
"""Real R PTY: three children queue on one ordinary provider slot, without paid calls."""
import argparse
import json
import ssl
import subprocess
import sys
import threading
from pathlib import Path

from acceptance_peer import Handler, Model, Server
from headless_acceptance import SESSION, require, wait, write
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_provider import calls, text
from sustained_walk import idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]
PARENT = 'SLOT_PARENT: delegate the three independent fixture tasks.'
TASKS = [f'SLOT_CHILD_{index}' for index in range(3)]


class BusyOnce(Handler):
    """One bounded HTTP refusal before the ordinary peer handles the retry."""
    def do_POST(self):
        with self.server.lock:
            first = self.server.requests == 0
            if first:
                self.server.requests += 1
        if not first:
            return super().do_POST()
        length = int(self.headers.get('Content-Length', '-1'))
        require(0 < length <= 8 * 1024 * 1024, 'rate-limit request byte bound')
        body = self.rfile.read(length)
        require(len(body) == length, 'truncated rate-limit request')
        self.server.dispatch(self.path, self.headers, json.loads(body))
        payload = b'{"error":{"type":"rate_limit_exceeded"}}'
        self.send_response(429)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload)))
        self.send_header('Retry-After', '10')
        self.end_headers()
        self.wfile.write(payload)


def backoff(terminal, journey, model, provider, peer, result):
    terminal.send('SLOT_MAIN: recover a temporary provider refusal')
    terminal.expect('Provider retrying')
    screen = terminal.capture('provider-refusal')
    require('Provider busy, retrying.' in screen, 'R omitted the provider-busy retry message')
    terminal.draft('keep this provider retry draft')
    terminal.capture('provider-busy-retrying-draft-kept')
    terminal.expect('SLOT_MAIN_RESPONSIVE')
    idle(journey)
    screen = terminal.capture('provider-recovered-draft-kept')
    require('keep this provider retry draftX' in screen, 'automatic retry lost the draft')
    require('Provider busy, retrying.' not in screen, 'completed retry left stale feedback')
    require(model.count() == 2 and provider.requests == 2, 'retry missing or replayed')
    require(not provider.errors and not peer.errors, 'loopback peer failed')
    terminal.close()
    result.update(passed=True, rate_limit_visible=True, automatic_retry=True,
                  draft_retained=True, provider_calls=model.count())


class SlotModel(Model):
    def __init__(self):
        super().__init__()
        self.release = [threading.Event() for _ in TASKS]
        self.order = []

    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        users = [json.dumps(row.get('content', '')) for row in body.get('input', [])
                 if row.get('role') == 'user']
        require(users, 'provider request has no user input')
        if PARENT in users[-1]:
            if any(row.get('call_id', '').startswith('call_slot_') and
                   row.get('type') == 'function_call_output' for row in body.get('input', [])):
                return text('ALL_SLOT_CHILDREN_DONE')
            return calls([(f'call_slot_{index}', 'subagent', {'task': task})
                          for index, task in enumerate(TASKS)])
        if 'SLOT_MAIN' in users[-1]:
            return text('SLOT_MAIN_RESPONSIVE')
        child = next((index for index, task in enumerate(TASKS) if task in users[-1]), None)
        require(child is not None, 'unexpected child prompt')
        with self.lock:
            require(child not in self.order, 'child provider attempt replayed')
            self.order.append(child)
        require(self.release[child].wait(timeout=45), 'held child was not released')
        return text(f'SLOT_CHILD_DONE_{child}')

    def started(self):
        with self.lock:
            return list(self.order)


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary product required')
    model, mail, journey, terminal = SlotModel(), TuiMail(), None, None
    provider = Server(0, model.dispatch)
    if args.backoff:
        provider.RequestHandlerClass = BusyOnce
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, source=args.source, ordinary_binaries=True, paid_calls=0)
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            # This profile is a Git project; executable MCP declarations belong
            # in this fixture's separate user configuration.
            project = json.loads((journey.profile / 'grapt.jsonc').read_text())
            write(Path(journey.environment['GRAPT_CONFIG_DIR']) / 'grapt.jsonc',
                  {'mcp': project.pop('mcp'),
                   'provider_capacity': {'children': 3}})
            # A project may serialize children, but cannot raise the user pool.
            project['provider_capacity'] = {'children': 64 if args.parallel else 1}
            project['permission']['subagent'] = 'ask'
            write(journey.profile / 'grapt.jsonc', project)
            config = journey.profile / 'service.conf'
            configuration = config.read_text()
            require(configuration.count('provider_active_attempts=') == 1, 'provider capacity missing or duplicated')
            config.write_text('\n'.join('provider_active_attempts=1' if line.startswith('provider_active_attempts=')
                else line for line in configuration.splitlines()) + '\n')
            journey.start(0)
            require(journey.provision().returncode == 0, 'disposable provision failed')
            terminal = RTerminal(args, journey)
            terminal.start(0)
            if args.backoff:
                return backoff(terminal, journey, model, provider, peer, result)
            terminal.send(PARENT)
            for index in range(3):
                def permission():
                    rows = journey.api('GET', f'/api/session/{SESSION}/permission')['data']
                    return next((row for row in rows if row['source'].get('callID') == f'call_slot_{index}'), None)
                ask = wait(permission, 'explicit subagent permission')
                journey.api('POST', f'/api/session/{SESSION}/permission/{ask["id"]}/reply', {'reply': 'once'})
            if args.parallel:
                wait(lambda: len(model.started()) == 3,
                     'three child requests run concurrently', seconds=3)
                main = 'ses_slot_independent_main'
                journey.api('POST', '/api/session', {'id': main, 'agent': 'agt_live_headless1',
                    'model': {'providerID': 'openai', 'id': 'gpt-live'},
                    'location': {'directory': str(journey.profile)}})
                journey.api('POST', f'/api/session/{main}/prompt', {
                    'id': 'msg_slot_main', 'text': 'SLOT_MAIN: stay responsive',
                    'delivery': 'queue', 'resume': True})
                wait(lambda: 'SLOT_MAIN_RESPONSIVE' in json.dumps(journey.api(
                    'GET', f'/api/session/{main}/message?order=asc&limit=100')) and
                    main not in journey.api('GET', '/api/session/active')['data'],
                    'main completes while all three children hold their slots', seconds=3)
                require(model.count() == 5, 'main did not obtain its independent slot')
                terminal.capture('three-children-running-main-responsive')
            else:
                wait(lambda: len(model.started()) == 1, 'one running child and two queued children')
                wait(lambda: terminal.drive('capture').count('waiting for provider slot') == 2,
                     'both queued children visible in R')
                screen = terminal.capture('two-children-waiting-for-provider-slot')
                require(screen.count('waiting for provider slot') == 2, 'R did not identify both waiting children')
                require('(position 1)' in screen and '(position 2)' in screen, 'R omitted FIFO positions')
            terminal.draft('keep this slot-wait draft')
            if args.parallel:
                for release in model.release:
                    release.set()
            else:
                require(len(model.started()) == 1, 'queued child reached the full provider')
                for index in range(3):
                    wait(lambda: len(model.started()) == index + 1, 'next child receives the provider slot')
                    model.release[model.started()[index]].set()
            terminal.expect('ALL_SLOT_CHILDREN_DONE')
            idle(journey)
            screen = terminal.capture('all-children-completed-draft-retained')
            require('keep this slot-wait draftX' in screen, 'provider progress lost the unsent draft')
            require('waiting for provider slot' not in screen, 'completed children retained waiting labels')
            rows = journey.api('GET', '/api/session')['data']
            require(sum(row.get('parentID') == SESSION for row in rows) == 3, 'three real child sessions required')
            require(model.count() == (6 if args.parallel else 5), 'unexpected provider attempt or missing parent continuation')
            require(not provider.errors and not peer.errors, 'loopback peer failed')
            terminal.close()
            result.update(passed=True, children=3, capacity=3 if args.parallel else 1,
                          parallel_children=args.parallel, wait_visible=not args.parallel, draft_retained=True,
                          provider_calls=model.count(), completion_order=model.started())
        finally:
            for release in model.release:
                release.set()
            if terminal:
                terminal.cleanup()
            if journey:
                journey.stop()
            write(args.scratch / 'result.json', result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binaries', type=Path, required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--parallel', action='store_true', help='require three simultaneous children and an independent main request')
    mode.add_argument('--backoff', action='store_true', help='show an HTTP 429 retry in ordinary R and retain the draft')
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
        '--scratch', str(args.scratch), *(['--parallel'] if args.parallel else []),
        *(['--backoff'] if args.backoff else [])], check=True, timeout=240)


if __name__ == '__main__':
    main()
