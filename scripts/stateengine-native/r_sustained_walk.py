#!/usr/bin/env python3
"""Bounded Linux R composer journey with ordinary binaries and disposable TLS peers."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys
from urllib.parse import urlencode

from acceptance_peer import CAPABILITY, PRIVATE_CURSOR, Server
from headless_acceptance import SESSION, require, wait, write, service_helper
from ordinary_profile import OrdinaryProfile
from sustained_provider import TASK, QUEUED, RESTART
from r_reliability_provider import ReliabilityModel, LOST, RECOVER
from r_recovery_journey import RecoveryProxy, point, retire, approval_disconnect, uncertain_restart, crash_phase
from sustained_walk import committed, idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail
from r_sustained_terminal import RTerminal

ROOT = Path(__file__).resolve().parents[2]


def checkpoint(args, name):
    observer = getattr(args, 'checkpoint', None)
    if observer:
        observer(name)


def verify_inputs(admissions):
    require(len(admissions) == 5 and len({row['inputID'] for row in admissions}) == 5,
            'lost or replayed input')
    require([row['input']['data']['text'] for row in admissions] == [TASK, QUEUED, RESTART, LOST, RECOVER],
            'admitted text differs from the edited composer input')
    require(all(row['sessionID'] == SESSION and row['input']['type'] == 'user' and
                row['input']['delivery'] == 'queue' for row in admissions), 'wrong producer input route')


def verify_approvals(rows, approved):
    asks = [value for kind, value in rows if kind == 'permission.asked.2']
    replies = [value for kind, value in rows if kind == 'permission.replied.2']
    expected = ['patch', 'shell', 'patch', 'patch', 'shell', 'shell', 'shell', 'shell']
    require([row['action'] for row in asks] == list(approved.values()) == expected,
            'missing or unexpected shell/patch approval')
    require([row['id'] for row in asks] == list(approved) == [row['requestID'] for row in replies]
            and all(row['reply'] == 'once' and row['sessionID'] == SESSION for row in replies),
            'approval identity or decision differs from the real R interaction')


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / (name + ('.exe' if os.name == 'nt' else '')) for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    loopback = getattr(args, 'loopback', False)
    version = command([args.product, '--version'])
    require(('testkit/fixture: loopback-provider-testkit' in version) if loopback else
        ('testkit/fixture' not in version), 'binary fixture label differs from the requested carrier')
    model, mail = ReliabilityModel(), TuiMail()
    provider = Server(0, model.dispatch)
    if not loopback:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
        provider.socket = context.wrap_socket(provider.socket, server_side=True)
    journey, terminal, proxy = None, None, None
    result = dict(passed=False, source=args.source, frontend='R', ordinary_binaries=not loopback,
        release_package='not measured', real_seat_day='not measured', paid_calls=0,
        windows='ConPTY' if os.name == 'nt' else 'not measured',
        native_mail_and_seat_binding='not measured' if getattr(args, 'session_only', False) else 'fixture')
    with provider, ExitStack() as faults:
        peer = None if getattr(args, 'session_only', False) else faults.enter_context(
            Server(4877, mail.dispatch, with_connection=True))
        try:
            journey = OrdinaryProfile(args, f'{"http" if loopback else "https"}://127.0.0.1:{provider.server_port}')
            if getattr(args, 'observe_profile', None):
                args.observe_profile(journey)
            config_path = journey.profile / 'grapt.jsonc'
            config = json.loads(config_path.read_text())
            config['permission'].update(shell='ask', edit='ask', patch='ask', nested_hold='allow')
            nested = dict(type='local',codemode=True,timeout=dict(execution=120000),command=[sys.executable,str(Path(__file__).with_name('nested_hold_fixture.py')),str(journey.profile)])
            if getattr(args, 'session_only', False):
                # prepare() creates a Git boundary at the service profile.
                # Executable fixture declarations belong to the synthetic
                # user's config, outside that repository's trust boundary.
                write(Path(journey.environment['GRAPT_CONFIG_DIR'])/'grapt.jsonc',
                      dict(mcp=dict(nested=nested)))
                config.pop('mcp')
            else:
                config['mcp']['nested'] = nested
            write(config_path, config)
            (journey.profile / 'calc.py').write_text('def add(a, b):\n    return abs(a) + abs(b)\n')
            (journey.profile / 'test_calc.py').write_text('import unittest\nimport calc\nclass Calc(unittest.TestCase):\n'
                '    def test_signed(self): self.assertEqual(calc.add(-2, 5), 3)\n'
                '    def test_zero(self): self.assertEqual(calc.add(0, 0), 0)\n')
            for n in range(8):
                (journey.profile / f'evidence-{n}.txt').write_text(f'evidence-{n}: signed addition task\n' + ('x'*79+'\n')*450)
            journey.start(0)
            proxy = faults.enter_context(RecoveryProxy(journey.endpoint))
            require(journey.provision().returncode == 0, 'disposable provision failed')
            point(journey, proxy)
            if getattr(args, 'session_only', False):
                query = urlencode({'location[directory]':str(journey.profile)})
                catalog = journey.api('GET', '/api/mcp/inspect?'+query)['data']
                require(any(row['server']['name']=='nested' and
                    row['server']['status']['status']=='connected' and
                    any(tool['name']=='nested_hold' and not tool.get('omission')
                        for tool in row['tools']) for row in catalog),
                    'private nested fixture is unavailable; no crash measurement is possible')
            terminal = getattr(args, 'terminal_class', RTerminal)(args, journey)
            terminal.start(0)
            if getattr(args, 'smoke', False):
                terminal.drive('send', 'y')
                terminal.expect('> y')
                terminal.drive('send', 'BSpace')
                terminal.close()
                retire(journey, proxy)
                checkpoint(args, 'native ConPTY startup and private Session')
                result.update(passed=True, smoke=True, complete_reliability_journey=False)
                return
            checkpoint(args, 'native ConPTY startup and private Session')
            terminal.send(TASK)
            approval_disconnect(terminal, journey, proxy)
            checkpoint(args, 'pending approval disconnect and explicit retry')
            terminal.until(lambda screen: 'SUSTAIN_TOOL_BEGIN' in screen, 'background output')
            terminal.capture('background-tool-output')
            terminal.draft(QUEUED)
            terminal.until(lambda _: model.summary_entered.is_set(), 'automatic compaction summary')
            terminal.capture('automatic-compaction')
            require(QUEUED not in json.dumps(model.bodies), 'unsent draft reached provider')
            terminal.edited(QUEUED)
            wait(lambda: len([1 for kind, _ in committed(journey) if kind == 'session.input.admitted.1']) == 2,
                 'R input admission during compaction')
            model.summary_release.set()
            terminal.until(lambda screen: 'SUSTAIN_QUEUED_DONE' in screen, 'queued work after compaction')
            idle(journey)
            terminal.capture('continued-after-compaction')
            require((journey.profile / 'worklog.txt').read_text() == 'addition verified; queued input completed once\n', 'queued work missing')
            rows = committed(journey)
            compactions = [value for kind, value in rows if kind == 'session.compaction.ended.2']
            require(compactions and all(value.get('trigger') == 'policyThreshold' for value in compactions), 'automatic compaction missing')
            before = journey.history()
            require('SUSTAIN_INITIAL_DONE' in json.dumps(before), 'original work did not complete')
            checkpoint(args, 'background tools, automatic compaction and queued edited input')
            terminal.draft(RESTART)
            pane = terminal.session
            old_endpoint = journey.endpoint
            old_credential = (journey.profile / 'basic.credential').read_bytes()
            proxy.offline = True
            retire(journey, proxy)
            terminal.until(lambda screen: 'Service' in screen and RESTART + 'X' in screen,
                           'offline service notice and retained draft')
            # Rotate only this fixture's authentication; Journey.api uses its owner.
            service_helper.SECRET += '-replacement'
            (journey.profile / 'basic.credential').write_text(
                f'{service_helper.USERNAME}:{service_helper.SECRET}', encoding='utf-8')
            journey.start(1)
            point(journey, proxy)
            require(journey.endpoint != old_endpoint and
                    (journey.profile / 'basic.credential').read_bytes() != old_credential,
                    'restart did not rotate endpoint and credential')
            require(journey.history() == before, 'restart changed committed history')
            journey.rebind()
            terminal.until(lambda screen: 'Connected' in screen and RESTART + 'X' in screen,
                           'original R reconnect with its edited draft')
            require(terminal.session == pane, 'restart replaced the R terminal process')
            require(len([1 for kind, _ in committed(journey)
                         if kind == 'session.input.admitted.1']) == 2,
                    'reconnect submitted an unsent draft or replayed earlier input')
            terminal.capture('same-r-reconnected-draft')
            checkpoint(args, 'same R reconnect with rotated endpoint, credential and retained draft')
            uncertain_restart(terminal, journey, proxy, model)
            terminal.until(lambda screen: 'SUSTAIN_RESTART_DONE' in screen, 'useful work after restart')
            idle(journey)
            terminal.capture('useful-work-after-restart')
            checkpoint(args, 'uncertain input explicit retry and repeated compaction continuation')
            crash_phase(terminal, journey, proxy, model)
            rows = committed(journey)
            compactions = [value for kind,value in rows if kind=='session.compaction.ended.2']
            require(len(compactions)==3 and all(value.get('trigger')=='policyThreshold' for value in compactions), 'three actual policy compactions required')
            checkpoint(args, 'three committed automatic compactions')
            retire(journey, proxy)
            terminal.close()
            rows = committed(journey)
            admissions = [value for kind, value in rows if kind == 'session.input.admitted.1']
            verify_inputs(admissions)
            verify_approvals(rows, terminal.approvals)
            require(not any(kind in ('session.execution.failed.1', 'session.execution.interrupted.1') for kind, _ in rows), 'producer failed')
            require(model.stage == 14 and len(model.handles) == 2 and not provider.errors and
                (peer is None or not peer.errors), 'tool sequence failed')
            require('return a + b' in (journey.profile / 'calc.py').read_text() and
                    'return a - b' in (journey.profile / 'calc.py').read_text(), 'useful changes lost')
            serialized = json.dumps(model.bodies)
            require(CAPABILITY not in serialized and PRIVATE_CURSOR not in serialized, 'private handles reached provider')
            checkpoint(args, 'clean offline exit and exact input, approval and effect settlement')
            result.update(passed=True, automatic_compactions=len(compactions), input_admissions=5,
                repeated_compaction_tests=True, uncertain_input_explicit_retry=True,
                pending_approval_disconnect=True, pending_question_crash_recovery=True,
                lost_nested_effect_reconciled_once=True,
                queued_edited_input=True, approvals=len(terminal.approvals), background_tool_output=True,
                continued_after_restart=True, same_r_process=True, rotated_endpoint_and_credential=True,
                retained_edited_restart_draft=True, offline_exit=True,
                physical_requests=model.count(), useful_tests_passed=True)
        finally:
            model.summary_release.set()
            model.restart_release.set()
            if terminal:
                if terminal.session:
                    try: terminal.capture('failure')
                    except (OSError, RuntimeError): pass
                terminal.cleanup()
            if journey:
                if journey.child and journey.child.poll() is None:
                    try: retire(journey, proxy)
                    except (OSError, RuntimeError, subprocess.TimeoutExpired): pass
                journey.cleanup()
                write(journey.root / 'model-requests.json', model.bodies)
                write(journey.root / 'observations.json', dict(actions=model.observed, errors=provider.errors,
                    mail_errors=None if peer is None else peer.errors, handles=model.handles))
                write(journey.root / 'events.json', committed(journey))
                if proxy:
                    write(journey.root / 'proxy-requests.json', proxy.requests)
            write(args.scratch / 'result.json', result)
    if proxy is None or proxy.errors:
        result['passed'] = False
        write(args.scratch / 'result.json', result)
        raise RuntimeError('transport fixture failed: ' + str(proxy.errors if proxy else 'not started'))
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
        '--scratch', str(args.scratch)], check=True, timeout=540)


if __name__ == '__main__':
    main()
