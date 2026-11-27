#!/usr/bin/env python3
"""Ordinary disposable Product/local MCP/R defaults and server-format validation journey."""
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
DRAFT = 'DEFAULTS COMPOSER DRAFT'
FORMATS = [('email', 'Expected email', 'seat@example.com'),
           ('uri', 'Expected URI', 'mailto:seat@example.com'),
           ('date', 'Expected date', '2024-02-29'),
           ('date-time', 'Expected date-time', 'December 17, 1995 03:24:00')]
DEFAULTS = dict(text='seed', count=2, ratio=1.25, flag=False, choice='b', many=['a', 'a'], zero=0)
FIELDS = [dict(key='text', type='string', default='seed', required=True),
    dict(key='count', type='integer', default=2, minimum=1, maximum=3, required=True),
    dict(key='ratio', type='number', default=1.25, minimum=0, maximum=2, required=True),
    dict(key='flag', type='boolean', default=False, required=True),
    dict(key='choice', type='string', default='b', custom=False, options=[dict(value='a', label='A'), dict(value='b', label='B')]),
    dict(key='many', type='multiselect', default=['a', 'a'], custom=False, options=[dict(value='a', label='A'), dict(value='b', label='B')]),
    dict(key='zero', type='integer', default=0)]


class DefaultsProxy(ElicitationProxy):
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
                identity = 'frm_default_' + name
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
            def formats(identity, global_form, keys):
                writes = len(proxy.form_writes)
                for index, (_, message, valid) in enumerate(FORMATS):
                    shortcut('s')
                    terminal.expect(message)
                    require(state(identity, global_form)['status'] == 'pending', 'invalid format was accepted')
                    terminal.drive('send', *['BSpace'] * 3)
                    terminal.drive('send', '--text', '\x1b[200~' + valid + '\x1b[201~')
                    terminal.expect(valid)
                    time.sleep(.15)
                    if index < len(FORMATS) - 1: terminal.drive('send', 'Tab')
                shortcut('s')
                terminal.expect('Decision recorded' if global_form else 'Answer confirmed')
                expected = dict(zip(keys, [valid for _, _, valid in FORMATS]))
                require(state(identity, global_form) == dict(status='answered', answer=expected), 'format answer changed')
                require(len(proxy.form_writes) == writes + 5, 'format validation or correction sent unexpected writes')
                dismiss()
                return expected
            with DefaultsProxy(journey.endpoint) as proxy:
                write(receipt, dict(readiness, observed_endpoint=proxy.endpoint))
                terminal.start(0)
                terminal.send('bootstrap')
                terminal.expect('ROW_A_PACKET_DONE')
                idle(journey)
                terminal.drive('send', '--text', DRAFT)
                identity = create('typed', FIELDS)
                terminal.expect('Defaults filled')
                writes = len(proxy.form_writes)
                time.sleep(2.2)
                require(len(proxy.form_writes) == writes and state(identity)['status'] == 'pending', 'default automatically submitted')
                resize(80, 16)
                terminal.expect('text * (string) = seed')
                require(any(line.strip('│ ').strip() == 'seed' for line in terminal.capture('short-default-text').splitlines()), 'default text editor hidden')
                shortcut('s')
                terminal.expect('Answer confirmed')
                require(state(identity) == dict(status='answered', answer=DEFAULTS), 'default JSON values or false/zero changed')
                resize(160, 45)
                dismiss()
                identity = create('edited', FIELDS)
                terminal.drive('send', '--text', ' edited')
                time.sleep(4.2)
                terminal.expect('seed edited')
                dismiss()
                terminal.drive('send', 'F6')
                terminal.expect('seed edited')
                terminal.drive('send', 'Tab', 'BSpace')
                terminal.drive('send', '--text', '4')
                writes = len(proxy.form_writes)
                shortcut('s')
                terminal.expect('Number is above maximum')
                require(len(proxy.form_writes) == writes, 'invalid default edit escaped bounds')
                terminal.drive('send', 'BSpace')
                terminal.drive('send', '--text', '3')
                terminal.drive('send', 'Tab', *['BSpace'] * 4)
                terminal.drive('send', '--text', '1.5')
                terminal.drive('send', 'Tab', 'Space', 'Tab', 'Down', 'Space', 'Tab', 'Space', 'Down', 'Space')
                shortcut('s')
                terminal.expect('Answer confirmed')
                expected = dict(DEFAULTS, text='seed edited', count=3, ratio=1.5, flag=True, choice='a', many=['b'])
                require(state(identity) == dict(status='answered', answer=expected), 'edited typed defaults changed')
                dismiss()
                keys = ['email', 'link', 'day', 'when']
                fields = [dict(key=key, type='string', default='bad', format=format, required=True)
                          for key, (format, _, _) in zip(keys, FORMATS)]
                identity = create('formats', fields)
                resize(80, 24)
                formats(identity, False, keys)
                resize(160, 45)
                identity = create('unknown', [dict(key='value', type='string', default='seed', placeholder='Unknown presentation semantics')])
                shortcut('s')
                terminal.expect('Unsupported field')
                require(state(identity)['status'] == 'pending', 'unknown semantics became an answer')
                shortcut('r')
                terminal.expect('Cancellation confirmed')
                dismiss()
                identity = ask('defaults')
                resize(80, 16)
                terminal.drive('send', *['Tab'] * 5)
                terminal.expect('text * (string) = seed')
                require(any(line.strip('│ ').strip() == 'seed' for line in terminal.capture('short-mcp-default').splitlines()), 'MCP default editor hidden')
                writes = len(proxy.form_writes)
                proxy.lose_next = proxy.hide_after_write = True
                shortcut('s')
                terminal.expect('Decision unconfirmed')
                shortcut('o')
                terminal.expect('State unreadable')
                time.sleep(2.2)
                require(len(proxy.form_writes) == writes + 1, 'default decision replayed')
                proxy.hidden = False
                shortcut('o')
                terminal.expect('Question already settled')
                expected_global = {key: value for key, value in DEFAULTS.items() if key != 'zero'}
                require(state(identity, True) == dict(status='answered', answer=expected_global), 'MCP defaults changed')
                resize(160, 45)
                dismiss()
                terminal.expect('MCP_FINISHED_defaults')
                idle(journey)
                identity = ask('formats')
                resize(80, 16)
                expected_formats = formats(identity, True, ['a_email', 'b_link', 'c_day', 'd_when'])
                resize(160, 45)
                terminal.expect('MCP_FINISHED_formats')
                idle(journey)
                require(log.stat().st_size <= 65536, 'MCP result byte bound')
                replies = [json.loads(line) for line in log.read_text().splitlines()]
                require(replies == [dict(case='defaults', result=dict(action='accept', content=expected_global)),
                    dict(case='formats', result=dict(action='accept', content=expected_formats))], 'real MCP response differed')
                require(model.count() == 5, 'form controls submitted a provider prompt')
                require(not proxy.errors and not provider.errors and not peer.errors, 'transport fixture failed')
                terminal.close()
                write(receipt, readiness)
                journey.stop()
                require(not receipt.exists(), 'clean Product stop did not retire readiness')
            terminal.cleanup()
            journey.cleanup()
            result.update(passed=True, typed_defaults_both_owners=True, edited_defaults_survive_refresh=True,
                defaults_never_auto_submit=True, canonical_format_success_and_refusal_both_owners=True,
                unknown_semantics_unresolved=True, uncertain_defaults_read_only_reconciliation=True,
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
