#!/usr/bin/env python3
"""Disposable ordinary CACHE6 producer comparison; no measured provider cache reuse."""
import argparse
from collections import deque
import json
from pathlib import Path
import socket
import ssl
import subprocess
import sys
import threading

ROOT = Path(__file__).resolve().parents[2]
from acceptance_peer import CAPABILITY, PRIVATE_CURSOR, Server, transport
from headless_acceptance import SESSION, require, wait, write
from ordinary_profile import OrdinaryProfile
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

FIXED = '''from copy import deepcopy

class State:
    def __init__(self): self.values={}
    def set(self,key,value): self.values[key]=deepcopy(value)
    def get(self,key): return deepcopy(self.values[key])
    def snapshot(self): return deepcopy(self.values)
'''
SUMMARY = '## Objective\n- Complete CACHE6 deep-copy ownership, preserving KeyError.\n## Next Steps\n- Continue every requested verification stage.'

class Producer:
    def __init__(self):
        self.lock, self.actions, self.bodies = threading.Lock(), deque(), []
        self.retry = False
        self.dropped = False
    def set(self, actions):
        with self.lock:
            require(not self.actions, 'unfinished scripted producer operation')
            self.actions.extend(actions)
    def dispatch(self, path, headers, body, connection):
        require(path == '/responses' and headers.get('Authorization') == 'Bearer disposable-model-dummy', 'unexpected fixture route')
        require(CAPABILITY not in json.dumps(body) and PRIVATE_CURSOR not in json.dumps(body), 'private binding reached model')
        with self.lock:
            require(len(self.bodies) < 32, 'provider request count bound')
            require(len(json.dumps(body).encode()) <= 1024 * 1024, 'provider request byte bound')
            self.bodies.append(body)
            require(self.actions, 'unscripted provider request')
            if self.retry and not self.dropped:
                self.dropped = True
                connection.shutdown(socket.SHUT_RDWR)
                return 'text/event-stream', b''
            kind, value = self.actions.popleft()
            if kind == 'tool':
                tool, arguments = value
                frames = transport.tool_call_frames(arguments, len(self.bodies), tool)
                clean = []
                for raw, _ in frames:
                    frame = json.loads(raw.removeprefix('data: ').strip())
                    frame.pop('usage', None)
                    if isinstance(frame.get('response'), dict): frame['response'].pop('usage', None)
                    clean.append('data: ' + json.dumps(frame) + '\n\n')
                return 'text/event-stream', ''.join(clean).encode()
            # No usage on any success, summary or failed connection: absence must remain absent.
            frames = [{'type':'response.output_text.delta', 'delta':value},
                {'type':'response.output_text.done'}, {'type':'response.completed', 'response':{}}]
            return 'text/event-stream', ''.join('data: ' + json.dumps(frame) + '\n\n' for frame in frames).encode()

def run(args):
    args.cli, args.product = [args.binaries / name for name in ('grapt', 'grapt-server')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source mismatch')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary binaries required')
    manifest = json.loads(args.task.read_text())
    if 'task' in manifest:
        source = manifest['task']
        require(source['id'] == 'CACHE6', 'wrong public continued task')
        prompts = json.loads(source['prompt'])['prompts']
    else:
        source = next(task for task in manifest['tasks'] if task['id'] == 'R01')
        commands = [
            "python3 -c \"from state import State; s=State(); v={'x':[1]}; s.set('k',v); v['x'].append(2); assert s.get('k')=={'x':[1]}\"",
            "python3 -c \"from state import State; s=State(); s.set('k',{'x':[1]}); q=s.snapshot(); q['k']['x'].append(2); assert s.get('k')=={'x':[1]}\"",
            "python3 -c \"from state import State; s=State(); s.set('k',{'x':[1]}); q=s.get('k'); q['x'].append(2); assert s.get('k')=={'x':[1]}\"",
            'python3 -m unittest discover',
            'git diff -- state.py; python3 -m unittest discover',
        ]
        prompts = [source['prompt'] + ' Run python3 -m unittest discover using the shell tool. Do not commit. Finish after verification.']
        prompts.extend(f'Verification stage {stage}/6. Use the shell tool to run exactly this command: {cmd}. Do not change files if it passes. Report the result briefly, then finish. Do not commit.'
            for stage, cmd in enumerate(commands, 2))
    require(len(prompts) == 6 and all(isinstance(text, str) and len(text.encode()) < 32768 for text in prompts), 'public prompt bound')
    require(set(source['files']) == {'app.py','state.py','README.md','test_public.py','.gitignore'}, 'public task files changed')
    producer, journey = Producer(), None
    provider = Server(0, producer.dispatch, with_connection=True)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, binary_source=args.source, paid_calls=0, ordinary_binaries=True,
        fixture_provider_usage='unreported', scripted_provider=True, provider_cache_read_share=None, real_seat_day='not measured')
    with provider, Server(4877, TuiMail().dispatch, with_connection=True) as mail:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            for name, text in source['files'].items():
                (journey.profile / name).write_text(text)
            for argv in [['git','init','-q'], ['git','add','--',*source['files']],
                ['git','-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','baseline']]:
                subprocess.run(argv, cwd=journey.profile, check=True, timeout=10, capture_output=True)
            journey.start(0)
            require(journey.provision().returncode == 0, 'disposable provision failed')
            def prompt(session, stage, actions):
                producer.set(actions)
                journey.api('POST', f'/api/session/{session}/prompt', dict(id=f'msg_cache6_stage_{stage}', text=prompts[stage-1], delivery='queue', resume=True))
                wait(lambda: not producer.actions and session not in journey.api('GET','/api/session/active')['data'], 'producer stage settles', seconds=45)
            def tool(name, arguments): return ('tool', (name, arguments))
            def checks(stage):
                cmd = prompts[stage-1].split('command: ',1)[1].split('. Do not change files',1)[0]
                return [tool('shell', dict(command=cmd, timeout_ms=10000)), ('text', f'CACHE6_STAGE_{stage}_DONE')]
            prompt(SESSION, 1, [tool('read', dict(path='state.py')),
                tool('edit', dict(path='state.py', old_text=source['files']['state.py'], new_text=FIXED)),
                tool('shell', dict(command='python3 -m unittest discover', timeout_ms=10000)), ('text','CACHE6_STAGE_1_DONE')])
            require((journey.profile / 'state.py').read_text() == FIXED, 'ordinary producer did not apply the ownership fix')
            producer.retry = True
            first_retry = len(producer.bodies)
            prompt(SESSION, 2, checks(2))
            require(producer.dropped and producer.bodies[first_retry] == producer.bodies[first_retry+1], 'retry changed request context')
            prompt(SESSION, 3, checks(3))
            journey.api('POST', f'/api/session/{SESSION}/compact', {})
            prompt(SESSION, 4, [('text', SUMMARY), *checks(4)])
            fork = journey.api('POST', f'/api/session/{SESSION}/fork', {})['data']['id']
            journey.api('PUT', f'/api/session/{fork}/stateengine/capability', dict(capability=CAPABILITY))
            prompt(fork, 5, checks(5))
            prompt(fork, 6, checks(6))
            require(len(producer.bodies) == 16, 'all producer, retry and summary calls did not run')
            for index, body in enumerate(producer.bodies):
                write(args.scratch / f'request-{index}.json', body)
            summary = producer.bodies[9]
            require(summary.get('prompt_cache_key') is None and len(summary['input']) == 2 and not summary.get('tools'), 'manual cold summary shape changed')
            coding = producer.bodies[:9] + producer.bodies[10:]
            fixed = lambda body: {key: value for key, value in body.items() if key != 'input'}
            require(all(fixed(body) == fixed(coding[0]) for body in coding), 'coding tools or fixed options changed')
            system = [item for item in coding[0]['input'] if item.get('role') == 'system']
            require(system and all(body['input'][:len(system)] == system for body in coding), 'coding System prefix changed')
            outputs = {item['call_id']: item['output'] for body in coding for item in body['input']
                if item.get('type') == 'function_call_output' and item.get('call_id', '').endswith('_shell')}
            require(len(outputs) == 6 and all(output.endswith('Command exited with code 0.') for output in outputs.values()), 'a producer verification command failed or was omitted')
            keys = [body.get('prompt_cache_key') for body in coding]
            require(keys[0] and all(key == keys[0] for key in keys), 'producer/fork coding cache body affinity changed')
            exported = []
            for session in [SESSION, fork]:
                raw = subprocess.run([args.product, 'usage','--service',journey.profile,'--session',session], cwd=journey.profile,
                    env=journey.environment, capture_output=True, check=True, timeout=30)
                report = json.loads(raw.stdout)
                require(report['enumeration_complete'], 'usage enumeration incomplete')
                require(len(report['attempts']) == (12 if session == SESSION else 4), 'usage crossed the fork ownership boundary')
                write(args.scratch / (session + '-usage.json'), report)
                exported.extend(report['attempts'])
            require(len(exported) == 16 and len({row['attempt'] for row in exported}) == 16, 'usage omitted a call or recounted inherited fork attempts')
            require(sum(row['operation'] == 'compaction' for row in exported) == 1 and all(row['step_terminal_observed'] for row in exported), 'summary usage or durable settlement is missing')
            require(all(row['usage']['input_total']['status'] == 'unavailable' and row['usage']['cache_read']['status'] == 'unavailable' for row in exported), 'fixture absence became a zero or observed cache share')
            # The public source stores a unittest module, so execute discovery as well.
            (journey.profile / 'test_cache6_hidden.py').write_text(source['hidden_tests'])
            verification = subprocess.run(['python3','-m','unittest','discover'], cwd=journey.profile, capture_output=True, timeout=15)
            require(verification.returncode == 0, 'public held-out ownership checks failed')
            require(not provider.errors and not mail.errors, 'fixture transport failed')
            journey.stop()
            require(not (journey.profile / 'serve-ready.json').exists(), 'Product did not retire readiness')
            journey.cleanup()
            result.update(passed=True, completed_public_stages=6, retry_context_unchanged=True, summary_executed=True,
                fork_continued=True, all_call_attempts=len(exported), all_usage_unavailable=True, clean_shutdown=True)
        finally:
            if journey:
                if journey.child and journey.child.poll() is None:
                    try: journey.stop()
                    except (OSError,RuntimeError,subprocess.TimeoutExpired): pass
                journey.cleanup()
            write(args.scratch / 'result.json', result)
    print(json.dumps(result),flush=True)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binaries',type=Path,required=True)
    parser.add_argument('--source',required=True)
    parser.add_argument('--scratch',type=Path,required=True)
    parser.add_argument('--task',type=Path,default=ROOT/'fixtures/benchmarks/tasks.json')
    parser.add_argument('--inside',action='store_true')
    args = parser.parse_args()
    require(args.task.is_file() and args.task.stat().st_size <= 1024*1024, 'public task must be a regular file within 1 MiB')
    if args.inside: return run(args)
    args.binaries,args.task,args.scratch=args.binaries.resolve(strict=True),args.task.resolve(strict=True),args.scratch.resolve()
    args.scratch.mkdir(mode=0o700)
    subprocess.run([sys.executable,str(ROOT/'scripts/native-tui/fixture-ca.py'),'--root',str(args.scratch/'tls')],check=True,timeout=30)
    subprocess.run(['bwrap','--die-with-parent','--new-session','--unshare-pid','--unshare-net','--ro-bind','/','/',
        '--tmpfs','/home','--tmpfs','/tmp','--tmpfs','/var/tmp','--proc','/proc','--dev','/dev',
        '--bind',str(args.scratch),str(args.scratch),'--ro-bind',str(ROOT),str(ROOT),'--chdir',str(args.scratch),
        '/usr/bin/env','-i','PATH=/usr/bin:/bin','PYTHONDONTWRITEBYTECODE=1',f'HOME={args.scratch}/empty-home',
        sys.executable,str(Path(__file__)), '--inside','--binaries',str(args.binaries),'--source',args.source,
        '--scratch',str(args.scratch),'--task',str(args.task)],check=True,timeout=300)
if __name__ == '__main__': main()
