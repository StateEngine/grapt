#!/usr/bin/env python3
"""Attended canonical Windows launch with real bound MCP calls from the R console.

The visible run requires the reservation and the independent readiness ACCEPT.
Only the model is scripted; the supplied bundled producer remains unmodified.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from types import SimpleNamespace

from acceptance_peer import Server
import headless_acceptance as headless
from bound_mcp_inputs import required_payload
from bound_mcp_model import BoundTools
from bundled_mcp_fixture import Fixture
from windows_console import Console, release_empty_controller
from stateengine_payload import manifest_name




def private_root(path):
    path = path.resolve()
    headless.require(path.parent == Path('E:/temp') and re.fullmatch(r'bound-mcp-[a-zA-Z0-9-]{1,60}', path.name),
                     'requires a fresh disposable E:/temp/bound-mcp-* root')
    headless.require(not path.exists(), 'scratch root already exists')
    return path


def isolated_environment(payload, scratch, inherited):
    environment = {key.upper(): value for key, value in inherited.items()
                   if key.upper() in ('PATH', 'SYSTEMROOT', 'WINDIR', 'COMSPEC', 'PATHEXT')}
    environment['PATH'] = str(payload) + os.pathsep + environment.get('PATH', '')
    environment.update(HOME=str(scratch / 'home'), USERPROFILE=str(scratch / 'home'),
        TMP=str(scratch / 'tmp'), TEMP=str(scratch / 'tmp'), TMPDIR=str(scratch / 'tmp'),
        APPDATA=str(scratch / 'appdata'), LOCALAPPDATA=str(scratch / 'localappdata'),
        XDG_CONFIG_HOME=str(scratch / 'config'), GIT_CONFIG_NOSYSTEM='1',
        GIT_CONFIG_GLOBAL=str(scratch / 'home/.gitconfig'), PYTHONDONTWRITEBYTECODE='1',
        STATEENGINE_REGISTRY_ROOT=str(scratch / 'home/registry'),
        STATEENGINE_OWNER_ROOT=str(scratch / 'home/owner'),
        STATEENGINE_KEYS_DIR=str(scratch / 'home/keys'))
    return environment


def run(args):
    if not re.fullmatch(r'agt-[a-z0-9-]{3,80}', args.owner):
        raise ValueError('requires the explicitly reserved owner identity')
    sys.path.insert(0, str(args.bench_kit))
    import processcap
    # This is the normal installed PC lease; reservations and deadline conflicts
    # refuse rather than retry. The job is installed before any preparation.
    lease = processcap.acquire_controller(owner=args.owner, purpose='attended bound-MCP Windows R',
        expires_unix=args.expires_unix, limit=32)
    result = dict(passed=False, cleanup_confirmed=False, go_message=args.go_msg,
                  readiness_accept=args.readiness_accept, controller=lease,
                  producer_unmodified=True, live_profile_touched=False,
                  existing_seat_day_load='present; existing seat and package untouched', paid_calls=0)
    journey = fixture = console = None
    created_root = False
    model = BoundTools()
    try:
        args.scratch = private_root(args.scratch)
        payload, producer = required_payload({'SECODE_STATEENGINE_PAYLOAD': str(args.payload)})
        result['producer_pin'] = dict(version=producer['version'], source_commit=producer['source_commit'],
            manifest_sha256=headless.digest(payload / manifest_name(payload)))
        headless.require(re.fullmatch('[0-9a-f]{40}', args.source), 'full source commit required')
        headless.require((args.binaries / 'source.txt').read_text().strip() == args.source,
                         'Windows fixture binaries source differs')
        headless.require(str(args.binaries.resolve()).lower().replace('\\', '/').startswith('e:/temp/gamma-'),
                         'use own disposable Windows fixtures, never the live Row A package')
        args.scratch.mkdir()
        created_root = True
        for name in ('home', 'tmp', 'config', 'appdata', 'localappdata'):
            (args.scratch / name).mkdir()
        environment = isolated_environment(payload, args.scratch, os.environ)
        # Public init creates its private registry; pre-creating it refuses with EEXIST.
        original = dict(os.environ)
        try:
            os.environ.clear()
            os.environ.update(environment)
            fixture = Fixture(payload, environment)
            fixture.start()
            with Server(0, model.dispatch) as provider:
                inputs = SimpleNamespace(scratch=args.scratch,
                    product=args.binaries / 'grapt-server.exe', cli=args.binaries / 'grapt.exe')
                version = subprocess.run([str(inputs.product), '--version'],
                    env=environment, capture_output=True, timeout=10, check=True)
                headless.require(b'testkit/fixture: loopback-provider-testkit' in version.stdout,
                                 'requires labelled loopback fixture binaries')
                journey = headless.Journey(inputs, 'windows-bound', provider.endpoint)
                journey.environment.update({key: environment[key] for key in
                    ('STATEENGINE_REGISTRY_ROOT', 'STATEENGINE_OWNER_ROOT', 'STATEENGINE_KEYS_DIR')})
                config_path = journey.profile / 'grapt.jsonc'
                config = json.loads(config_path.read_text())
                config.pop('mcp')
                config['permission']['execute'] = 'allow'
                config['stateengine'] = {'native_mail_endpoint': fixture.mail_endpoint}
                headless.write(config_path, config)
                journey.packet.unlink()  # Only the canonical producer's actual delivered packet is used.
                journey.start(0)
                fixture.launch(journey, args.binaries / 'grapt-tui.exe')
                headless.SESSION = fixture.session
                model.engine, model.binding, model.packet = fixture, fixture.binding, fixture.packet
                console = Console(args.binaries / 'grapt-tui.exe', journey.profile,
                                  fixture.session, processcap._JOB)
                (args.scratch / 'r-pid.txt').write_text(str(console.pid) + '\n')
                console.expect('Connected')
                console.expect('enter send')
                (args.scratch / 'connected.txt').write_text(console.screen(), encoding='utf-8')
                console.send('Verify the admitted bound MCP tools.\r')
                headless.wait(lambda: 'BOUND_REAL_STATEENGINE_DONE' in json.dumps(journey.history()),
                              'real bound MCP tools completed from original R input', seconds=180)
                headless.require(len(model.checks) == 9 and all(model.checks.values()),
                                 model.diagnostics.get('failure', 'required Windows bound-MCP check failed'))
                headless.require(not provider.errors and console.alive(),
                                 'provider failed or original R exited before completion')
                (args.scratch / 'completed.txt').write_text(console.screen(), encoding='utf-8')
                result.update(passed=True, source=args.source, producer=producer['source_commit'],
                              native_r_pid=console.pid, checks=model.checks)
        finally:
            os.environ.clear()
            os.environ.update(original)
    except BaseException as error:
        result.update(error=type(error).__name__ + ': ' + str(error)[:2048])
        raise
    finally:
        errors = []
        # Retained native process handle prevents numeric PID reuse during cleanup.
        for action in ([console.stop, console.close] if console else []) + (
                [fixture.stop_seat] if fixture else []) + (
                [journey.stop, journey.cleanup] if journey else []) + (
                [fixture.close] if fixture else []):
            try:
                action()
            except BaseException as error:
                errors.append(type(error).__name__ + ': ' + str(error)[:512])
        if console:
            try:
                headless.wait(console.window_closed, 'owned console window closed', seconds=5)
            except BaseException as error:
                errors.append(type(error).__name__ + ': ' + str(error)[:512])
        if not errors:
            try:
                release_empty_controller(processcap._JOB, processcap._LOCK.close)
            except BaseException as error:
                errors.append(type(error).__name__ + ': ' + str(error)[:512])
        result.update(passed=result['passed'] and not errors, cleanup_confirmed=not errors, cleanup_errors=errors,
                      checks=model.checks, diagnostics=model.diagnostics)
        if created_root:
            headless.write(args.scratch / 'result.json', result)
        # Failed cleanup/census retains the lease and its expiry guard until
        # process exit, when the kernel kills the remaining owned descendants.
        headless.require(not errors, 'owned Windows fixture cleanup failed')
    return 0 if result['passed'] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('binaries', 'payload', 'scratch', 'bench-kit'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--owner', required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--go-msg', required=True)
    parser.add_argument('--readiness-accept', required=True)
    parser.add_argument('--expires-unix', type=float, required=True)
    args = parser.parse_args()
    headless.require(os.name == 'nt', 'requires native Windows')
    headless.require(120 <= args.expires_unix - time.time() <= 900, 'hard expiry must be 2–15 minutes away')
    headless.require(all(re.fullmatch(r'msg-[a-zA-Z0-9]+', value)
                         for value in (args.go_msg, args.readiness_accept)), 'actual GO and ACCEPT message ids required')
    raise SystemExit(run(args))


if __name__ == '__main__':
    main()
