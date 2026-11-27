#!/usr/bin/env python3
"""Disposable ordinary Product/local MCP/R PTY elicitation and restart journey."""
import argparse
from collections import deque
import json
from pathlib import Path
import ssl
import subprocess
import sys
import time
from urllib.error import HTTPError
from urllib.parse import urlencode, urlsplit

from acceptance_peer import Model, Server, transport
from headless_acceptance import require, wait, write
from ordinary_profile import OrdinaryProfile
from r_forms_walk import FormProxy
from r_sustained_terminal import RTerminal
from sustained_walk import idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail, text_frames

ROOT = Path(__file__).resolve().parents[2]
DRAFT = 'MCP UNSENT COMPOSER'


class ElicitationModel(Model):
    def __init__(self):
        super().__init__()
        self.actions = deque()

    def ask(self, case):
        with self.lock:
            require(not self.actions, 'unfinished model action')
            self.actions.extend([('ask', case), ('complete', case)])

    def dispatch(self, path, headers, body):
        fallback = super().dispatch(path, headers, body)
        with self.lock:
            action = self.actions.popleft() if self.actions else None
        if action is None:
            return fallback
        kind, case = action
        if kind == 'complete':
            require('MCP_DONE_' + case in json.dumps(body), 'MCP response did not reach model')
            return text_frames('MCP_FINISHED_' + case)
        require(any(tool.get('name') == 'elicitor_ask' for tool in body.get('tools', [])), 'real elicitor tool missing')
        frames = transport.tool_call_frames(dict(case=case), 0, 'elicitor_ask')
        return 'text/event-stream', ''.join(frame for frame, _ in frames).encode()


class ElicitationProxy(FormProxy):
    def __init__(self, endpoint):
        self.offline = False
        super().__init__(endpoint)

    def faults(self, method, path):
        if self.offline: return True, False
        path = urlsplit(path).path
        refused, dropped = super().faults(method, path)
        hidden = self.hidden and method == 'GET' and path == '/api/form/request'
        return refused or hidden, dropped


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source mismatch')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary Product required')
    model, journey, terminal = ElicitationModel(), None, None
    provider = Server(0, model.dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, source=args.source, paid_calls=0, ordinary_binaries=True)
    readiness = None
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
            def route(identity, suffix=''):
                return '/api/session/global/form/' + identity + suffix + query
            def state(identity):
                return journey.api('GET', route(identity, '/state'))['data']
            def replies():
                if not log.exists(): return []
                require(log.stat().st_size < 65536, 'MCP evidence bound')
                return [json.loads(line) for line in log.read_text().splitlines()]
            def shortcut(ch):
                terminal.drive('send', '--text', chr(ord(ch) - ord('a') + 1))
            def dismiss():
                terminal.drive('send', 'Escape')
                terminal.expect(DRAFT)
                require(any(line.strip('│ ').strip() == '> ' + DRAFT for line in terminal.capture('retained-composer').splitlines()), 'question changed composer draft')
            def ask(case, first=False):
                if not first:
                    terminal.drive('send', *['BSpace'] * len(DRAFT))
                model.ask(case)
                terminal.send('Request MCP ' + case)
                wait(lambda: any(line.strip('│ ').strip() == '>' for line in terminal.drive('capture').splitlines()), 'prompt acknowledgement clears submitted composer')
                terminal.drive('send', '--text', DRAFT)
                def discovered():
                    rows = journey.api('GET', '/api/form/request' + query)['data']
                    return next((row for row in rows if row.get('metadata', {}).get('message') == 'MCP question ' + case), None)
                row = wait(discovered, 'real global elicitation')
                require(row['sessionID'] == 'global', 'invented Session owner')
                terminal.expect('MCP question ' + case)
                terminal.expect(str(journey.profile))
                terminal.capture('question-' + case)
                return row['id']
            with ElicitationProxy(journey.endpoint) as proxy:
                write(receipt, dict(readiness, observed_endpoint=proxy.endpoint))
                terminal.start(0)
                terminal.send('bootstrap')
                terminal.expect('ROW_A_PACKET_DONE')
                idle(journey)
                identity = ask('answer', first=True)
                command(['tmux', 'resize-window', '-t', terminal.session, '-x', '80', '-y', '16'])
                # Properties are canonical JSON order: choice, then count.
                terminal.drive('send', 'Down', 'Space', 'Tab')
                terminal.drive('send', '--text', '3')
                terminal.expect('count * (integer) = 3')
                require(any(line.strip('│ ').strip() == '3' for line in terminal.capture('short-global-editor').splitlines()), 'short viewport hid the numeric editor')
                command(['tmux', 'resize-window', '-t', terminal.session, '-x', '160', '-y', '45'])
                shortcut('s')
                terminal.expect('Decision recorded')
                require(state(identity) == dict(status='answered', answer=dict(choice='preview', count=3)), 'typed answer changed')
                wait(lambda: len(replies()) == 1, 'live MCP answer')
                require(replies()[0] == dict(case='answer', result=dict(action='accept', content=dict(choice='preview', count=3))), 'live MCP typed response differs')
                dismiss()
                terminal.expect('MCP_FINISHED_answer')
                idle(journey)
                identity = ask('decline')
                shortcut('r')
                terminal.expect('Decision recorded')
                wait(lambda: len(replies()) == 2, 'live MCP decline')
                require(replies()[1] == dict(case='decline', result=dict(action='cancel')), 'MCP decline differs')
                dismiss()
                terminal.expect('MCP_FINISHED_decline')
                idle(journey)
                identity = ask('lost')
                wrong = args.scratch / 'other-location'
                wrong.mkdir()
                subprocess.run(['git', 'init', '-q', str(wrong)], check=True, timeout=5)
                for method, suffix, body in [('GET', '', None), ('GET', '/state', None),
                        ('POST', '/reply', dict(answer=dict(branch='foreign'))), ('POST', '/cancel', {})]:
                    foreign = '/api/session/global/form/' + identity + suffix + '?' + urlencode({'location[directory]': str(wrong)})
                    try:
                        journey.api(method, foreign, body)
                    except HTTPError as error:
                        require(error.code == 404, 'wrong location did not refuse with 404')
                    else:
                        raise RuntimeError('another location accessed a global question')
                require(state(identity)['status'] == 'pending', 'foreign response settled question')
                terminal.drive('send', '--text', 'retained answer')
                proxy.lose_next = proxy.hide_after_write = True
                shortcut('s')
                terminal.expect('Decision unconfirmed')
                writes = len(proxy.form_writes)
                shortcut('o')
                terminal.expect('State unreadable')
                time.sleep(2.2)
                require(len(proxy.form_writes) == writes, 'uncertain reply automatically replayed')
                require(state(identity) == dict(status='answered', answer=dict(branch='retained answer')), 'lost reply not recorded')
                proxy.hidden = False
                shortcut('o')
                terminal.expect('Question already settled')
                require(len(proxy.form_writes) == writes, 'read-only check sent reply')
                wait(lambda: len(replies()) == 3, 'lost-response MCP delivery')
                dismiss()
                terminal.expect('MCP_FINISHED_lost')
                idle(journey)
                identity = ask('restart')
                terminal.drive('send', '--text', 'unsubmitted global draft')
                writes_before_restart = len(proxy.form_writes)
                calls_before_restart = model.count()
                requests = Path(str(log) + '.requests')
                require(requests.read_text().splitlines() == ['answer', 'decline', 'lost', 'restart'], 'MCP request sequence changed')
                pane = terminal.session
                proxy.offline = True
                write(receipt, readiness)
                journey.stop()
                require(not receipt.exists(), 'readiness not retired on clean stop')
                journey.start(1)
                readiness = json.loads(receipt.read_text())
                target = urlsplit(journey.endpoint)
                proxy.target = (target.hostname, target.port)
                write(receipt, dict(readiness, observed_endpoint=proxy.endpoint))
                proxy.offline = False
                terminal.until(lambda screen: 'Connected' in screen, 'same R reconnect')
                require(terminal.session == pane, 'restart replaced the R terminal')
                shortcut('o')
                terminal.expect('suspended')
                terminal.expect('unsubmitted global draft')
                require(state(identity)['status'] == 'suspended', 'restart did not suspend lost waiter')
                dismiss()
                terminal.drive('send', 'F6')
                terminal.expect('unsubmitted global draft')
                time.sleep(2.2)
                require(model.count() == calls_before_restart and requests.read_text().splitlines() == ['answer', 'decline', 'lost', 'restart'], 'restart re-executed provider or MCP work')
                require(len(proxy.form_writes) == writes_before_restart and len(replies()) == 3, 'restart replayed an old response')
                shortcut('r')
                terminal.expect('suspended MCP wait was not resumed')
                require(state(identity)['status'] == 'cancelled', 'explicit suspended decision not recorded')
                require(len(proxy.form_writes) == writes_before_restart + 1 and len(replies()) == 3, 'lost waiter was replayed')
                terminal.capture('recorded-without-waiter')
                terminal.close()
                write(receipt, readiness)
                journey.stop()
                require(not receipt.exists(), 'restarted Product did not retire readiness')
                require(not proxy.errors, 'transport fixture failed: ' + str(proxy.errors))
            require(not provider.errors and not peer.errors, 'provider/MCP fixture failed')
            require('stateengine_resume' in (journey.root / 'mcp-calls.txt').read_text(), 'real MCP bootstrap missing')
            terminal.cleanup()
            journey.cleanup()
            result.update(passed=True, real_mcp_typed_answer_and_decline=True, location_refusal=True,
                composer_preserved=True, short_viewport_editor_visible=True,
                uncertain_reply_read_only_reconciliation=True,
                clean_restart_no_replay=True, same_r_preserves_global_and_composer_drafts=True,
                suspended_decision_does_not_resume_waiter=True,
                clean_product_shutdown=True)
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
