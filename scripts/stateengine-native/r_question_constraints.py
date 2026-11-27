#!/usr/bin/env python3
"""Ordinary disposable Product/local MCP/R constraints and canonical validation journey."""
import argparse
import json
from pathlib import Path
import ssl
import subprocess
import sys
import time
from urllib.parse import urlencode
from urllib.error import HTTPError

from acceptance_peer import Server
from headless_acceptance import SESSION, require, wait, write
from ordinary_profile import OrdinaryProfile
from r_mcp_elicitation_walk import ElicitationModel, ElicitationProxy
from r_sustained_terminal import RTerminal
from sustained_walk import idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]
DRAFT = 'CONSTRAINTS COMPOSER DRAFT'
BRANCH = [dict(key='enabled', type='boolean', default=True, required=True),
    dict(key='branch', type='string', default='release', required=True,
         when=[dict(key='enabled', op='eq', value=True)]),
    dict(key='detail', type='string', default='kept', required=True,
         when=[dict(key='branch', op='eq', value='release')]),
    dict(key='other', type='string', default='retained',
         when=[dict(key='branch', op='neq', value='preview')])]
CUSTOM = [dict(key='one', type='string', default='a', required=True, custom=True,
    options=[dict(value='a', label='Offered A'), dict(value='b', label='Offered B')]),
    dict(key='many', type='multiselect', default=['a'], required=True, custom=True,
         minItems=2, maxItems=2, options=[dict(value='a', label='Offered A')])]


class ConstraintsProxy(ElicitationProxy):
    max_requests = 512


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source mismatch')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary Product required')
    model, journey, terminal, readiness = ElicitationModel(), None, None, None
    provider = Server(0, model.dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, source=args.source, paid_calls=0, ordinary_binaries=True)
    with provider, Server(4877, TuiMail().dispatch, with_connection=True) as peer:
        try:
            log = args.scratch / 'elicitation-replies.jsonl'
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}', dict(elicitor=dict(
                type='local', codemode=False, command=[sys.executable,
                    str(Path(__file__).with_name('elicitation_fixture.py')), str(log)])))
            journey.start(0)
            require(journey.provision().returncode == 0, 'disposable provision failed')
            terminal = RTerminal(args, journey)
            receipt = journey.profile / 'serve-ready.json'
            readiness = json.loads(receipt.read_text())
            query = '?' + urlencode({'location[directory]': str(journey.profile)})
            session_base = '/api/session/' + SESSION + '/form/'
            def route(identity, suffix='', global_form=False):
                return ('/api/session/global/form/' if global_form else session_base) + identity + suffix + (query if global_form else '')
            def state(identity, global_form=False):
                return journey.api('GET', route(identity, '/state', global_form))['data']
            def shortcut(ch):
                terminal.drive('send', '--text', chr(ord(ch) - ord('a') + 1))
            def resize(width, height):
                command(['tmux', 'resize-window', '-t', terminal.session, '-x', str(width), '-y', str(height)])
            def dismiss():
                terminal.drive('send', 'Escape')
                wait(lambda: 'F6/Esc close' not in terminal.drive('capture'), 'question overlay closes')
                terminal.expect(DRAFT)
                require(any(line.strip('│ ').strip() == '> ' + DRAFT for line in terminal.capture('retained-composer').splitlines()), 'question changed composer')
            def create(name, fields):
                identity = 'frm_constraints_' + name.replace('-', '_')
                try:
                    journey.api('POST', session_base.rstrip('/'), dict(id=identity, title='Default question ' + name, fields=fields))
                except HTTPError as error:
                    raw = error.read(8193)
                    require(len(raw) <= 8192, 'error body bound')
                    raise RuntimeError('Product form creation refused: ' + raw.decode('utf-8', errors='replace')) from error
                terminal.expect('Default question ' + name)
                return identity
            def ask(case):
                terminal.drive('send', *['BSpace'] * len(DRAFT))
                model.ask(case)
                terminal.send('Request MCP ' + case)
                wait(lambda: any(line.strip('│ ').strip() == '>' for line in terminal.drive('capture').splitlines()), 'prompt acknowledgement')
                terminal.drive('send', '--text', DRAFT)
                def discovered():
                    return next((row for row in journey.api('GET', '/api/form/request' + query)['data']
                        if row.get('metadata', {}).get('message') == 'MCP question ' + case), None)
                row = wait(discovered, 'real elicitation')
                require(row['sessionID'] == 'global', 'MCP owner was converted to a Session')
                terminal.expect('MCP question ' + case)
                return row['id']
            with ConstraintsProxy(journey.endpoint) as proxy:
                write(receipt, dict(readiness, observed_endpoint=proxy.endpoint))
                terminal.start(0)
                terminal.send('bootstrap')
                terminal.expect('ROW_A_PACKET_DONE')
                idle(journey)
                terminal.drive('send', '--text', DRAFT)
                identity = create('branches', BRANCH)
                terminal.drive('send', 'Tab', 'Tab')
                terminal.drive('send', '--text', ' edited')
                terminal.expect('kept edited')
                terminal.drive('send', 'BTab', 'BTab', 'Space')
                time.sleep(2.2)
                screen = terminal.capture('hidden-branches')
                require('kept edited' not in screen and 'release' not in screen and 'retained' not in screen, 'inactive drafts or descendants remained visible')
                terminal.drive('send', 'Tab')
                terminal.expect('> enabled')
                terminal.drive('send', 'Space', 'Tab', 'Tab')
                terminal.expect('kept edited')
                dismiss()
                terminal.drive('send', 'F6')
                terminal.expect('kept edited')
                resize(80, 16)
                require(any(line.strip('│ ').strip() == 'kept edited' for line in terminal.capture('short-restored-branch').splitlines()), 'restored branch editor hidden')
                terminal.drive('send', 'BTab', 'BTab', 'Space')
                terminal.expect('enabled * (boolean) = false')
                writes = len(proxy.form_writes)
                proxy.lose_next = proxy.hide_after_write = True
                shortcut('s')
                terminal.expect('Decision unconfirmed')
                terminal.drive('send', 'Space', 'Tab')
                shortcut('e')
                terminal.drive('send', '--text', 'forbidden uncertain edit')
                shortcut('o')
                terminal.expect('State unreadable')
                time.sleep(2.2)
                require(len(proxy.form_writes) == writes + 1, 'conditional decision replayed')
                proxy.hidden = False
                shortcut('o')
                terminal.expect('Question already settled')
                require(state(identity) == dict(status='answered', answer=dict(enabled=False)), 'inactive values escaped or uncertainty changed the original answer')
                resize(160, 45)
                dismiss()
                for name, size in [('focus-normal', (160, 45)), ('focus-short', (80, 16))]:
                    identity = create(name, [dict(key=key, type=kind, custom=True,
                        options=[dict(value='a', label='A'), dict(value='b', label='B')])
                        for key, kind in [('one', 'string'), ('many', 'multiselect')]])
                    resize(*size)
                    def choose_custom(value, previous=''):
                        shortcut('e')
                        if previous: terminal.drive('send', *['BSpace'] * len(previous))
                        terminal.drive('send', '--text', value)
                        terminal.drive('send', 'Enter')
                    choose_custom('a')
                    terminal.drive('send', 'Space', 'Tab')
                    choose_custom('a')
                    terminal.drive('send', 'Space')
                    for value, previous in [('custom-one', 'a'), ('custom-two', 'custom-one'), ('custom-one', 'custom-two')]:
                        choose_custom(value, previous)
                    terminal.drive('send', 'Space')
                    shortcut('s')
                    terminal.expect('Answer confirmed')
                    require(state(identity) == dict(status='answered', answer=dict(one='a', many=['custom-two'])),
                            'Space acted on a different row after a custom commit')
                    resize(160, 45)
                    dismiss()
                identity = create('custom', CUSTOM)
                writes = len(proxy.form_writes)
                shortcut('e')
                terminal.drive('send', 'BSpace')
                terminal.drive('send', '--text', 'custom-single')
                shortcut('s')
                terminal.expect('Enter chooses custom text')
                require(len(proxy.form_writes) == writes, 'uncommitted custom text submitted')
                terminal.drive('send', 'Tab', 'BTab')
                shortcut('e')
                terminal.expect('custom-single')
                terminal.drive('send', 'Enter', 'Tab')
                resize(80, 16)
                shortcut('e')
                terminal.drive('send', '--text', 'custom-multi')
                require(any(line.strip('│ ').strip() == 'custom-multi' for line in terminal.capture('short-custom-entry').splitlines()), 'custom text editor hidden')
                terminal.drive('send', 'Enter')
                terminal.expect('Custom value chosen')
                terminal.drive('send', 'Space')
                shortcut('s')
                terminal.expect('Too few selections')
                require(len(proxy.form_writes) == writes, 'custom minimum was ignored')
                shortcut('e')
                terminal.drive('send', 'Enter')
                shortcut('s')
                terminal.expect('Answer confirmed')
                require(state(identity) == dict(status='answered', answer=dict(one='custom-single', many=['a', 'custom-multi'])), 'custom values or offered selection changed')
                resize(160, 45)
                dismiss()
                identity = create('pattern', [dict(key='code', type='string', default='bad', required=True, pattern=r'^(a)\1$')])
                resize(80, 16)
                shortcut('s')
                terminal.expect('does not match pattern')
                require(state(identity)['status'] == 'pending', 'invalid pattern answer settled')
                terminal.drive('send', *['BSpace'] * 3)
                terminal.drive('send', '--text', 'aa')
                shortcut('s')
                terminal.expect('Answer confirmed')
                require(state(identity) == dict(status='answered', answer=dict(code='aa')), 'canonical backreference was not used')
                resize(160, 45)
                dismiss()
                for name, field in [('invalid-pattern', dict(key='code', type='string', default='x', pattern='(')), ('external', dict(key='ack', type='external', url='https://example.test'))]:
                    identity = create(name, [field])
                    shortcut('s')
                    terminal.expect('invalid pattern' if name == 'invalid-pattern' else 'Unsupported field')
                    require(state(identity)['status'] == 'pending', 'unsupported semantics silently settled')
                    shortcut('r')
                    terminal.expect('Cancellation confirmed')
                    dismiss()
                identity = create('custom-decline', CUSTOM)
                shortcut('e')
                terminal.drive('send', '--text', 'never selected')
                shortcut('r')
                terminal.expect('Cancellation confirmed')
                require(state(identity)['status'] == 'cancelled', 'custom entry could not be declined')
                dismiss()
                identity = ask('defaults')
                resize(80, 16)
                shortcut('s')
                terminal.expect('Decision recorded')
                expected_global = dict(text='seed', count=2, ratio=1.25, flag=False, choice='b', many=['a', 'a'])
                require(state(identity, True) == dict(status='answered', answer=expected_global), 'MCP regression')
                resize(160, 45)
                dismiss()
                terminal.expect('MCP_FINISHED_defaults')
                idle(journey)
                before = journey.api('GET', '/api/form/request' + query)['data']
                terminal.drive('send', *['BSpace'] * len(DRAFT))
                model.ask('unsupported-pattern')
                terminal.send('Request MCP unsupported-pattern')
                wait(lambda: any(line.strip('│ ').strip() == '>' for line in terminal.drive('capture').splitlines()), 'prompt acknowledged')
                terminal.drive('send', '--text', DRAFT)
                terminal.expect('MCP_FINISHED_unsupported-pattern')
                idle(journey)
                require(journey.api('GET', '/api/form/request' + query)['data'] == before, 'unsupported MCP semantics became a form')
                terminal.expect(DRAFT)
                require(log.stat().st_size <= 65536, 'MCP result byte bound')
                replies = [json.loads(line) for line in log.read_text().splitlines()]
                require(len(replies) == 2 and replies[0] == dict(case='defaults', result=dict(action='accept', content=expected_global)), 'MCP typed values changed')
                require(replies[1] == dict(case='unsupported-pattern', error=dict(code=-32602, message='The elicitation request could not be accepted')), 'unsupported MCP schema was not explicitly refused')
                require(model.count() == 5, 'form controls submitted a provider prompt')
                require(not proxy.errors and not provider.errors and not peer.errors, 'transport fixture failed')
                terminal.close()
                write(receipt, readiness)
                journey.stop()
                require(not receipt.exists(), 'clean Product stop did not retire readiness')
            terminal.cleanup()
            journey.cleanup()
            result.update(passed=True, branching_and_restoration=True, inactive_values_omitted=True,
                explicit_custom_single_and_multiselect=True, canonical_pattern_refusal_and_correction=True,
                unsupported_mcp_schema_refused=True, uncertain_decision_read_only_reconciliation=True,
                composer_preserved=True, normal_and_short_viewports=True, clean_shutdown=True)
        finally:
            if terminal:
                if terminal.session:
                    try: terminal.capture('failure')
                    except (OSError, RuntimeError): pass
                terminal.cleanup()
            if journey:
                if journey.child and journey.child.poll() is None:
                    if readiness: write(journey.profile / 'serve-ready.json', readiness)
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
