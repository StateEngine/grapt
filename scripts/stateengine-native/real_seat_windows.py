#!/usr/bin/env python3
"""Real Windows seat door, private roots, scripted model, and exact assertion lines.

Run the windowed phase only from the attended desktop command. The supplied
package is unchanged; --fixture-binaries supplies its same-source product built
with the existing loopback-provider-testkit feature. No system CA is installed.
"""
import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from urllib.parse import urlencode

from acceptance_peer import Server
from headless_acceptance import service_helper, write
from real_seat_control import Commands, api, find_session, fresh_root, stop_engine
from real_seat_model import SeatModel
from real_seat_inputs import verify_fixture
from real_seat_faults import FAULTS, LaunchProxy, job_command, report_control, selected_config
from real_seat_report import Journal
from real_seat_stability import observe_stability


def prepare_profile(control, product, endpoint):
    control.guard()
    if control.deadline - time.monotonic() < 65:
        raise TimeoutError('insufficient deadline for bounded profile initialization')
    inputs = control.root / 'inputs'
    inputs.mkdir()
    config = {
        'store': {'authority': 'initialize'},
        'product': {'build': 'real-seat-surface-witness'},
        'provider': {'id': 'openai', 'model': 'gpt-live', 'base_model': 'gpt-live',
            'family': 'gpt-5', 'route': 'route-native', 'account': 'native-fixture',
            'credential_version': 'fixture-v1', 'policy_version': 'fixture-v1', 'variants': []},
        'catalog': {'source': 'models.dev-chatgpt'}, 'compaction': {'server_side': False},
        'permission': {'stateengine_*': 'allow', 'shell': 'allow'},
    }
    write(inputs / 'grapt.jsonc', config)
    (inputs / 'provider.credential').write_text('disposable-model-dummy', encoding='utf-8')
    write(inputs / 'model-catalog.json', {'openai': {'id': 'openai', 'name': 'Fixture',
        'npm': '@ai-sdk/openai', 'models': {'gpt-live': {'id': 'gpt-live', 'name': 'gpt-live',
            'family': 'gpt-5', 'tool_call': True,
            'modalities': {'input': ['text'], 'output': ['text']}, 'limit': {'output': 32000}}}}})
    write(inputs / 'account-models.json', {'models': [{'slug': 'gpt-live', 'visibility': 'list',
        'context_window': 128000, 'supported_reasoning_levels': []}]})
    control.environment['GRAPT_LIVE_HEADLESS_LOOPBACK'] = endpoint
    profile = control.root / 'profile'
    original = dict(os.environ)
    try:
        os.environ.clear()
        os.environ.update(control.environment)
        service_helper.prepare(profile, inputs, product, 600000)
    finally:
        os.environ.clear()
        os.environ.update(original)
    control.guard()
    return profile


def declared_inputs(args, control):
    path = args.package / 'release-manifest.json'
    if path.stat().st_size > 2 * 1024 * 1024:
        raise ValueError('package manifest exceeded 2 MiB')
    manifest = json.loads(path.read_text(encoding='utf-8'))
    source = manifest['engine_version']
    if manifest['target'] != 'x86_64-pc-windows-msvc':
        raise ValueError('the supplied package must target native Windows')
    for name in ('grapt.exe', 'grapt-tui.exe'):
        binary = args.package / name
        if not binary.is_file() or binary.is_symlink():
            raise ValueError('package entrypoint absent: ' + name)
        if binary.stat().st_size != manifest['binaries'][name]['bytes']:
            raise ValueError('package entrypoint size differs: ' + name)
        with binary.open('rb') as stream:
            digest = hashlib.file_digest(stream, 'sha256').hexdigest()
        if digest != manifest['binaries'][name]['sha256']:
            raise ValueError('package entrypoint differs from its manifest: ' + name)
    product = verify_fixture(args.fixture_binaries, source)
    worker = Path(__file__).resolve().parents[1] / 'release/windows_version_worker.py'
    _, version = control.run('fixture-version', [sys.executable, worker, product], timeout=15)
    if 'testkit/fixture: loopback-provider-testkit' not in version:
        raise ValueError('fixture carrier must use the existing loopback-only feature')
    actual = subprocess.check_output(['git', '-C', str(args.stateengine), 'rev-parse', 'HEAD'],
                                     timeout=10, text=True).strip()
    if actual != args.stateengine_commit:
        raise ValueError('StateEngine checkout differs from the requested candidate')
    dirt = subprocess.check_output(['git', '--no-optional-locks', '-c', 'core.fsmonitor=false',
        '-C', str(args.stateengine), 'status', '--porcelain', '--untracked-files=no'], timeout=10)
    if dirt:
        raise ValueError('StateEngine candidate checkout must be clean')
    return source, product


def observe_mcp(endpoint, workspace, journal, checks):
    query = urlencode({'location[directory]': str(workspace)})
    try:
        value = api(endpoint, 'GET', '/api/mcp/inspect?' + query)
        rows = [row for row in value['data'] if row['server']['name'] == 'stateengine']
        states = [row['server']['status'] for row in rows]
        passed = len(rows) == 1 and states[0].get('status') == 'connected'
        details = dict(statuses=states)
    except Exception as error:
        passed, details = False, dict(error=type(error).__name__, reason=str(error)[:1024])
    line = journal.record('generated_mcp_config_trusted', passed=passed, **details)
    checks['generated_mcp_config_trusted'] = passed, line


def observe_subscription(root, session, journal, checks):
    path = root / 'product.stderr'
    if not path.is_file() or path.stat().st_size > 2 * 1024 * 1024:
        return
    for number, line in enumerate(path.read_text(encoding='utf-8').splitlines(), 1):
        if line == 'native mail subscription opened session=' + session:
            observed = journal.record('native_mail_subscribed', passed=True, source=path.name,
                                      source_line=number, exact_line=line)
            checks['native_mail_subscribed'] = True, observed
            return
        if 'native mail unavailable session=' + session in line:
            observed = journal.record('native_mail_subscribed', passed=False, source=path.name,
                                      source_line=number, exact_line=line)
            checks['native_mail_subscribed'] = False, observed


def matching_readiness(profile, child):
    if child.poll() is not None:
        raise RuntimeError('private product exited before readiness')
    path = profile / 'serve-ready.json'
    if not path.is_file():
        return None
    if path.stat().st_size > 8192:
        raise ValueError('product readiness exceeded 8 KiB')
    try:
        ready = json.loads(path.read_text(encoding='utf-8'))
    except json.JSONDecodeError:
        return None
    return ready if ready.get('pid') == child.pid else None


def run(args):
    root = fresh_root(args.scratch)
    journal = Journal(root)
    control = Commands(root, args.stateengine, args.node, journal,
                       time.monotonic() + args.timeout_seconds)
    job = root / 'exit-three.py'
    job.write_text('import time\ntime.sleep(8)\nraise SystemExit(3)\n', encoding='utf-8')
    command, shell = job_command(control, sys.executable, job, args.fault)
    marker = 'REAL_SEAT_MAIL_' + root.name
    model = SeatModel(journal, marker, command, job_shell=shell, job_root=control.workspace)
    engine = attempted = False
    session = None
    endpoint = None
    cleanup_ok = True
    completed = False
    launch_ok = False
    child = None
    logs = []
    checks = model.checks
    source = None
    try:
        source, product = declared_inputs(args, control)
        journal.record('candidate_pair', grapt_source=source, stateengine=args.stateengine_commit,
                       provider='same-source loopback fixture', package_unchanged=True)
        with Server(0, model.dispatch) as provider, ExitStack() as transports:
            _, initialized = control.se('init', ['init', '--project', control.project,
                '--repo-root', str(control.workspace), '--registry-root', str(control.registry),
                '--agent-name', 'Private seat witness', '--json'])
            identity = json.loads(initialized)['profile']
            agent, human = identity['agent_id'], identity['human_id']
            model.expected_principal = control.project, agent
            control.declare_mail(args.node)
            mcp_path = control.prepare_mcp_path(agent)
            selected = selected_config(control, mcp_path, args.fault)
            engine = True
            control.se('up', ['up', *control.binding, '--detach', '--no-follow', '--json'], timeout=120)
            journal.record('private_engine_started', project=control.project)
            profile = prepare_profile(control, product, provider.endpoint)
            out, err = [(root / ('product.' + suffix)).open('xb') for suffix in ('stdout', 'stderr')]
            logs.extend((out, err))
            child = subprocess.Popen([str(product), 'serve-terminal', '--service', str(profile), '--port', '0'],
                cwd=root, env=control.environment, stdin=subprocess.PIPE, stdout=out, stderr=err)
            ready = control.wait(lambda: matching_readiness(profile, child),
                         'private product readiness', seconds=20)
            endpoint = ready['observed_endpoint']
            proxy = transports.enter_context(LaunchProxy(endpoint, control, mcp_path, selected, args.fault))
            write(profile / 'serve-ready.json', dict(ready, observed_endpoint=proxy.endpoint))
            config = root / 'seat-config.json'
            write(config, {'project': control.project, 'projectRoot': str(control.workspace),
                'seats': [{'agent_id': agent, 'surface': 'grapt',
                    'command': str(args.package / 'grapt.exe'),
                    'grapt_tui_binary': str(args.package / 'grapt-tui.exe'),
                    'grapt_service': str(profile), 'grapt_workspace': str(control.workspace),
                    'grapt_agent': 'agt_live_headless1', 'model': 'openai/gpt-live'}]})
            attempted = True
            code, launched = control.se('launch', ['seat', 'launch', agent, *control.binding,
                '--surface', 'grapt', '--machine', 'local', '--config', str(config), '--json'],
                timeout=120, required=False)
            journal.record('real_windowed_launch_returned', exit_code=code)
            if code == 0:
                receipt = json.loads(launched)
                if Path(receipt['grapt_mcp_config']).resolve() != mcp_path:
                    raise ValueError('real launch published a different MCP config path')
                launch_ok = True
            # Inspect even a refused launch, so a trust failure stays named.
            observe_mcp(endpoint, control.workspace, journal, checks)
            if launch_ok and not proxy.applied:
                raise ValueError('real launch did not traverse the provision observation barrier')
            session = find_session(endpoint)
            api(endpoint, 'POST', '/api/session/' + session + '/prompt',
                {'text': 'REAL_SEAT_BOOT: verify this private launch, then wait for mail.'})
            journal.record('single_bootstrap_prompt')
            control.wait(lambda: model.initial_idle, 'first real model turn', seconds=30)
            control.wait(lambda: session not in api(endpoint, 'GET', '/api/session/active')['data'],
                 'durable idle before mail', seconds=10)
            model.initial_idle_confirmed = True
            journal.record('idle_observed_before_mail', session=session)
            _, sent = control.se('send-mail', ['msg', 'send', '--project', control.project,
                '--project-root', str(control.workspace), '--registry-root', str(control.registry),
                '--user-id', human, '--to', agent, '--kind', 'question', '--body', marker, '--json'])
            sent = json.loads(sent)
            message = sent.get('msg_id', sent.get('message', {}).get('msg_id'))
            if not isinstance(message, str) or not message.startswith('msg-'):
                raise ValueError('mail send returned no message identity')
            with model.serial:
                if model.context.get('mail_id') not in (None, message):
                    raise ValueError('model read a different mail identity from the sent receipt')
                model.context['mail_id'] = message
            journal.record('human_mail_sent_to_private_seat', project=control.project, recipient=agent)
            control.wait(lambda: model.job_idle, 'seat ended the job-start turn', seconds=30)
            control.wait(lambda: session not in api(endpoint, 'GET', '/api/session/active')['data'],
                 'durable idle before job completion', seconds=5)
            model.job_idle_confirmed = True
            journal.record('idle_observed_after_job_start', session=session)
            began = time.monotonic()
            try:
                control.wait(lambda: model.context.get('job_notice'), 'automatic job completion wake', seconds=60)
            except TimeoutError:
                elapsed = time.monotonic() - began
                if args.fault == 'job-project-missing' and model.job_idle_confirmed and elapsed >= 60:
                    model.observed('background_job_wakes_idle_seat', False, reason='no new completion input')
                    journal.record('job_wake_absence_observed', idle_seconds=elapsed)
                raise
            control.guard()
            observe_subscription(root, session, journal, checks)
            observe_stability(control, session, child, journal, checks)
            if provider.errors or proxy.errors:
                raise RuntimeError('fixture transport refused a request: ' + (provider.errors + proxy.errors)[0])
            completed = True
    except BaseException as error:
        journal.record('witness_refused', kind=type(error).__name__, reason=str(error)[:2048])
    finally:
        if attempted:
            try:
                control.se('seat-stop', ['seat', 'stop', agent, *control.binding, '--json'],
                           timeout=30, cleanup=True)
            except Exception as error:
                cleanup_ok = False
                journal.record('seat_stop_failed', kind=type(error).__name__)
        if session is not None:
            try:
                observe_subscription(root, session, journal, checks)
            except Exception as error:
                cleanup_ok = False
                journal.record('subscription_observer_failed', kind=type(error).__name__)
        if child:
            try:
                child.stdin.close()
                child.wait(timeout=15)
                if child.returncode != 0:
                    cleanup_ok = False
                    journal.record('product_stop_failed', exit_code=child.returncode)
            except (OSError, subprocess.TimeoutExpired) as error:
                cleanup_ok = False
                journal.record('product_cleanup_failed', kind=type(error).__name__)
                try:
                    child.kill()
                    child.wait(timeout=5)
                except (OSError, subprocess.TimeoutExpired) as stop_error:
                    journal.record('product_survived_cleanup', kind=type(stop_error).__name__)
        for stream in logs:
            stream.close()
        if engine:
            try:
                stop_engine(control)
            except Exception as error:
                cleanup_ok = False
                journal.record('private_engine_stop_failed', kind=type(error).__name__)
        passed = journal.report(checks) and cleanup_ok and completed and launch_ok
        report_control(root, args.fault, journal, checks)
        write(root / 'result.json', dict(passed=passed, cleanup_confirmed=cleanup_ok,
                                        completed=completed, launch_confirmed=launch_ok,
                                        fault=args.fault, paid_calls=0, source=source,
                                        stateengine=args.stateengine_commit))
        journal.close()
    return 0 if passed else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', type=Path, required=True)
    parser.add_argument('--fixture-binaries', type=Path, required=True)
    parser.add_argument('--stateengine', type=Path, required=True)
    parser.add_argument('--stateengine-commit', required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--node', type=Path, required=True)
    parser.add_argument('--timeout-seconds', type=int, default=480)
    parser.add_argument('--fault', choices=FAULTS, default='none')
    args = parser.parse_args()
    if os.name != 'nt' or not 120 <= args.timeout_seconds <= 600:
        parser.error('requires native Windows and a 120–600 second bound')
    if not re.fullmatch('[0-9a-f]{40}', args.stateengine_commit):
        parser.error('--stateengine-commit requires the full candidate commit')
    for name in ('package', 'fixture_binaries', 'stateengine', 'node'):
        setattr(args, name, getattr(args, name).resolve(strict=True))
    return run(args)


if __name__ == '__main__':
    raise SystemExit(main())
