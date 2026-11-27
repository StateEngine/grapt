#!/usr/bin/env python3
"""Ordinary R/Product/MCP form interaction in a disposable private Linux PTY."""
import argparse
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys
import time

from acceptance_peer import Model, Server
from headless_acceptance import SESSION, require, write
from launch_proxy import Proxy
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_walk import idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]
DRAFT = 'UNSENT COMPOSER DRAFT'


class FormProxy(Proxy):
    def __init__(self, endpoint):
        self.fail_next = self.lose_next = self.hide_after_write = False
        self.hidden = self.hide_state = False
        self.form_writes = []
        super().__init__(endpoint)

    def faults(self, method, path):
        decision = method == 'POST' and '/form/' in path and path.endswith(('/reply', '/cancel'))
        if decision:
            self.form_writes.append(path)
            if self.fail_next:
                self.fail_next = False
                return True, False
            if self.lose_next:
                self.lose_next = False
                self.hidden = self.hide_after_write
                return False, True
        hidden_read = method == 'GET' and (path.endswith('/form') or '/form/' in path)
        state_read = method == 'GET' and '/form/' in path and path.endswith('/state')
        return self.hidden and hidden_read or self.hide_state and state_read, False


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source mismatch')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary Product required')
    model, mail, journey, terminal = Model(), TuiMail(), None, None
    provider = Server(0, model.dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, source=args.source, paid_calls=0, ordinary_binaries=True)
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            journey.start(0)
            require(journey.provision().returncode == 0, 'disposable provision failed')
            terminal = RTerminal(args, journey)
            receipt = journey.profile / 'serve-ready.json'
            readiness = json.loads(receipt.read_text())
            base = '/api/session/' + SESSION + '/form'
            def create(name, title, fields):
                identity = 'frm_rforms_' + name
                value = journey.api('POST', base, dict(id=identity, title=title, fields=fields))
                require(value['data']['id'] == identity, 'created another form identity')
                terminal.expect(title)
                return identity
            def state(identity):
                return journey.api('GET', base + '/' + identity + '/state')['data']
            def shortcut(ch):
                terminal.drive('send', '--text', chr(ord(ch) - ord('a') + 1))
            def close_question():
                terminal.drive('send', 'Escape')
                terminal.expect(DRAFT)
            string = dict(key='branch', type='string', required=True)
            with FormProxy(journey.endpoint) as proxy:
                write(receipt, dict(readiness, observed_endpoint=proxy.endpoint))
                terminal.start(0)
                terminal.drive('send', '--text', 'forms bootstrap')
                terminal.drive('send', 'Enter')
                terminal.expect('ROW_A_PACKET_DONE')
                idle(journey)
                terminal.drive('send', '--text', DRAFT)
                terminal.expect(DRAFT)

                identity = create('answer', 'Choose a branch', [string, dict(key='dry_run', type='boolean', required=True)])
                close_question()
                terminal.drive('send', 'F6')
                terminal.expect('Choose a branch')
                shortcut('s')
                terminal.expect('Still needed: branch')
                terminal.drive('send', '--text', '\x1b[200~main\nsecond line\x1b[201~')
                terminal.drive('send', 'Tab')
                terminal.drive('send', 'Space')
                shortcut('s')
                terminal.expect('Answer confirmed')
                require(state(identity) == dict(status='answered', answer=dict(branch='main\nsecond line', dry_run=True)), 'typed/pasted fields changed')
                close_question()

                writes = len(proxy.form_writes)
                identity = create('numeric', 'Numeric question', [
                    dict(key='count', type='integer', required=True, minimum=1, maximum=3),
                    dict(key='ratio', type='number', minimum=-2, maximum=0),
                    dict(key='optional', type='number')])
                terminal.drive('send', '--text', '\x1b[200~1.5\x1b[201~')
                shortcut('s')
                terminal.expect('Enter an integer')
                terminal.drive('send', *['BSpace'] * 3)
                terminal.drive('send', '--text', '4')
                shortcut('s')
                terminal.expect('Number is above maximum')
                terminal.drive('send', 'BSpace')
                terminal.drive('send', '--text', '3')
                terminal.drive('send', 'Tab')
                terminal.drive('send', '--text', '\x1b[200~-2.5\x1b[201~')
                shortcut('s')
                terminal.expect('Number is below minimum')
                terminal.drive('send', *['BSpace'] * 4)
                terminal.drive('send', '--text', '\x1b[200~-1.25\x1b[201~')
                terminal.drive('send', 'Tab')
                terminal.drive('send', '--text', 'oops')
                shortcut('s')
                terminal.expect('Enter a finite JSON number: optional')
                terminal.drive('send', 'Tab', 'Tab', 'Tab')
                terminal.expect('draft: oops')
                screen = terminal.capture('retained-invalid-number')
                require(any(line.strip('│ ').strip() == 'oops' for line in screen.splitlines()), 'invalid numeric editor text was lost across fields')
                require(len(proxy.form_writes) == writes and state(identity)['status'] == 'pending', 'invalid numeric draft sent a response')
                terminal.drive('send', *['BSpace'] * 4)
                terminal.drive('send', '--text', '0')
                proxy.lose_next = True
                shortcut('s')
                terminal.expect('Answer confirmed')
                numeric = state(identity)
                require(numeric == dict(status='answered', answer=dict(count=3, ratio=-1.25, optional=0)), 'numeric values changed')
                require([type(numeric['answer'][key]) for key in ('count', 'ratio', 'optional')] == [int, float, int], 'numeric JSON types or zero changed')
                require(len(proxy.form_writes) == writes + 1, 'numeric lost reply replayed')
                close_question()

                identity = create('choices', 'Choice question', [
                    dict(key='one', type='string', required=True, custom=False, options=[
                        dict(value='branch-stable', label='Stable release'), dict(value='branch-canary', label='Preview release')]),
                    dict(key='many', type='multiselect', required=True, custom=False, minItems=2, maxItems=2, options=[
                        dict(value='lint', label='Lint checks'), dict(value='test', label='Unit tests'), dict(value='build', label='Build package')])])
                writes = len(proxy.form_writes)
                terminal.expect('Preview release')
                terminal.drive('send', 'Down')
                shortcut('s')
                terminal.expect('Still needed: one')
                terminal.drive('send', 'Space', 'Tab', 'Space')
                shortcut('s')
                terminal.expect('Too few selections')
                terminal.drive('send', 'Down', 'Space', 'Down', 'Space')
                shortcut('s')
                terminal.expect('Too many selections')
                require(len(proxy.form_writes) == writes, 'invalid selection sent a response')
                terminal.drive('send', 'Space', 'BTab')
                close_question()
                terminal.drive('send', 'F6')
                terminal.expect('branch-canary')
                proxy.fail_next = True
                shortcut('s')
                terminal.expect('Decision not confirmed')
                terminal.drive('send', 'Up', 'Space')
                time.sleep(2.2)
                require(len(proxy.form_writes) == writes + 1, 'selection response retried automatically')
                shortcut('s')
                terminal.expect('Answer confirmed')
                require(state(identity) == dict(status='answered', answer=dict(one='branch-canary', many=['lint', 'test'])), 'option labels, types or frozen choices changed')
                close_question()

                for name, title, field in [
                    ('external', 'Unsupported external question', dict(key='link', type='external', url='https://example.com')),
                    ('unknown', 'Unsupported placeholder question', dict(string, placeholder='Unknown semantics'))]:
                    identity = create(name, title, [field])
                    terminal.drive('send', '--text', '\x1b[200~unoffered\x1b[201~')
                    shortcut('s')
                    terminal.expect('Unsupported field')
                    require(state(identity)['status'] == 'pending', 'unsupported field was settled')
                    shortcut('r')
                    terminal.expect('Cancellation confirmed')
                    require(state(identity)['status'] == 'cancelled', 'explicit decline did not cancel')
                    close_question()

                identity = create('refused', 'Retry question', [string])
                terminal.drive('send', '--text', 'original choice')
                proxy.fail_next = True
                shortcut('s')
                terminal.expect('Decision not confirmed')
                writes = len(proxy.form_writes)
                time.sleep(2.2)
                require(len(proxy.form_writes) == writes and state(identity)['status'] == 'pending', 'failed write retried automatically')
                terminal.drive('send', '--text', '\x1b[200~changed\x1b[201~')
                shortcut('s')
                terminal.expect('Answer confirmed')
                require(state(identity) == dict(status='answered', answer=dict(branch='original choice')), 'retry changed original decision')
                close_question()

                identity = create('lost', 'Lost reply question', [string])
                terminal.drive('send', '--text', 'retained lost choice')
                proxy.lose_next = proxy.hide_after_write = True
                shortcut('s')
                terminal.expect('Decision unconfirmed')
                writes = len(proxy.form_writes)
                shortcut('o')
                terminal.expect('State unreadable')
                time.sleep(2.2)
                require(len(proxy.form_writes) == writes, 'read check replayed the decision')
                require(state(identity) == dict(status='answered', answer=dict(branch='retained lost choice')), 'lost reply did not reach canonical form')
                proxy.hidden = False
                proxy.hide_state = True
                # The settled form disappears from outstanding rows while its
                # draft remains unconfirmed. Neither dismissal may steal focus
                # back from the composer on subsequent successful list polls.
                time.sleep(4.4)
                for dismiss in ('Escape', 'F6'):
                    terminal.drive('send', dismiss)
                    time.sleep(4.4)
                    terminal.drive('send', '--text', 'x')
                    terminal.expect('> ' + DRAFT + 'x')
                    screen = terminal.capture('dismissed-' + dismiss.lower())
                    require(any(line.strip('│ ').strip() == '> ' + DRAFT + 'x' for line in screen.splitlines()), 'unchanged form polls captured composer input after ' + dismiss)
                    terminal.drive('send', 'BSpace')
                    terminal.drive('send', 'F6')
                    terminal.expect('Lost reply question')
                proxy.hide_state = False
                shortcut('o')
                terminal.expect('Question already settled')
                terminal.expect('answered: branch=retained lost choice')
                require(len(proxy.form_writes) == writes, 'positive reconciliation replayed the decision')
                close_question()

                identity = create('elsewhere', 'Settled elsewhere question', [string])
                terminal.drive('send', '--text', 'local unsent answer')
                journey.api('POST', base + '/' + identity + '/reply', dict(answer=dict(branch='external answer')))
                writes = len(proxy.form_writes)
                shortcut('s')
                terminal.expect('Question already settled')
                terminal.expect('answered: branch=external answer')
                require(len(proxy.form_writes) == writes, 'stale form posted an answer')
                close_question()
                screen = terminal.capture('retained-composer')
                require(any(line.strip('│ ').strip() == '> ' + DRAFT for line in screen.splitlines()), 'form flow changed the composer')
                prompt_writes = [path for method, path in proxy.requests if method == 'POST' and path.endswith('/prompt')]
                require(len(prompt_writes) == 1 and model.count() == 1, 'form keys submitted a composer prompt')
                require(not proxy.errors, 'transport fixture failure: ' + str(proxy.errors))
                write(journey.root / 'form-writes.json', proxy.form_writes)
                terminal.close()
                write(receipt, readiness)
                journey.stop()
                require(not receipt.exists(), 'Product did not retire its readiness receipt')
            require(not provider.errors and not peer.errors, 'provider/MCP fixture failure')
            require('stateengine_resume' in (journey.root / 'mcp-calls.txt').read_text(), 'real MCP bootstrap missing')
            terminal.cleanup()
            journey.cleanup()
            result.update(passed=True, delayed_discovery=True, typed_and_pasted_fields=True,
                explicit_answer_and_decline=True, composer_preserved=True,
                unsupported_types_and_constraints_unresolved=True, failed_write_manual_retry=True,
                uncertain_write_read_only_reconciliation=True, dismissed_uncertain_form_preserves_focus=True,
                stale_form_no_write=True, clean_product_shutdown=True,
                typed_numeric_bounds_and_invalid_drafts=True, explicit_single_and_multi_choice=True,
                typed_decision_retry_preserves_original=True)
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
        '--scratch', str(args.scratch)], check=True, timeout=300)


if __name__ == '__main__':
    main()
