#!/usr/bin/env python3
"""Run one native PC qualification leg while the gate runs its Linux commands."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time
import uuid

# A landing sidecar owns operational code; the candidate owns test coverage.
if __name__ == '__main__' and '--source-policy' in sys.argv:
    os.environ['PYTHONDONTWRITEBYTECODE'] = '1'
    import importlib.util
    policy_parser = argparse.ArgumentParser(add_help=False)
    policy_parser.add_argument('--source-policy', type=Path, required=True)
    policy_options, _ = policy_parser.parse_known_args()
    policy_root = policy_options.source_policy.resolve()
    if policy_root != Path.cwd().resolve() or not (policy_root/'.git').exists():
        raise ValueError('Native policy must belong to the selected checkout')
    for name in ('test_features', 'slow_tests', 'windows_gate'):
        path = policy_root/'scripts/build'/(name+'.py')
        if not path.is_file() or path.is_symlink() or path.stat().st_size > 65536:
            raise ValueError('Native policy must be a bounded regular module')
        specification = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(specification)
        sys.modules[name] = module
        specification.loader.exec_module(module)

from nightly_revert import quote, remote
import windows_gate
from windows_native_result import read, verify, MAX_LOG, CAPACITY_EXIT, CapacityUnavailable


_STOP_REQUESTED = False


def interrupt(_signal, _frame):
    global _STOP_REQUESTED
    if not _STOP_REQUESTED:
        _STOP_REQUESTED = True
        raise KeyboardInterrupt


def git(root, *args):
    return subprocess.check_output(['git', *args], cwd=root, timeout=60).decode().strip()


def version_worker():
    helpers = Path(__file__).resolve().parent
    selected = helpers/'windows_version_worker.py'  # Private landing transport.
    if not selected.is_file():
        selected = helpers.parent/'release/windows_version_worker.py'
    read(selected, 65536)
    return selected


def transfer(source, destination):
    subprocess.run(['scp', '-q', str(source), os.environ['SECODE_WINDOWS_HOST']+':/'+destination], check=True, timeout=300)


def collect(pc, name, destination, maximum):
    amount = int(remote('(Get-Item -LiteralPath '+quote(pc+'/'+name)+').Length', timeout=30).strip())
    if not 0 <= amount <= maximum:
        raise ValueError('Native Windows output exceeds its bound: '+name)
    subprocess.run(['scp', '-q', os.environ['SECODE_WINDOWS_HOST']+':/'+pc+'/'+name, str(destination)], check=True, timeout=120)
    return read(destination, maximum)


def wait(pc, deadline):
    while time.monotonic() < deadline:
        state = remote('[IO.File]::WriteAllText('+quote(pc+'/heartbeat')+',[DateTime]::UtcNow.Ticks);'
                       'if(Test-Path -LiteralPath '+quote(pc+'/done.json')+')'
                       '{Get-Content -LiteralPath '+quote(pc+'/done.json')+' -Raw}', timeout=30).strip()
        if state:
            if len(state) > 128:
                raise ValueError('Native completion marker exceeds its bound')
            return json.loads(state)['exit']
        time.sleep(2)
    raise TimeoutError('Native Windows qualification exceeded its deadline')


def task_arguments(pc):
    return ('--headless C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe '
            '-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "'+pc+'/windows_native.ps1" -RunRoot "'+pc+'"')


def start(pc, task, timeout):
    # OpenSSH may close the job containing its descendants when SSH exits.
    # Task Scheduler gives this owned worker an independent bounded lifecycle.
    remote('$name='+quote(task)+';if(Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue)'
           '{throw "Preserve existing native task"};'
           '$a=New-ScheduledTaskAction -Execute "C:/Windows/System32/conhost.exe"'
           ' -Argument '+quote(task_arguments(pc))+';'
           '$p=New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name)'
           ' -LogonType Interactive -RunLevel Limited;'
           '$s=New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Seconds '+str(timeout+120)+');'
           'Register-ScheduledTask -TaskName $name -Action $a -Principal $p -Settings $s|Out-Null;'
           '[IO.File]::WriteAllText('+quote(pc+'/heartbeat')+',[DateTime]::UtcNow.Ticks);'
           'Start-ScheduledTask -TaskName $name', timeout=30)


def remove_task(pc, task):
    remote('$t=Get-ScheduledTask -TaskName '+quote(task)+' -ErrorAction SilentlyContinue;'
           'if($t){if($t.Actions.Count -ne 1 -or $t.Actions[0].Execute -cne "C:/Windows/System32/conhost.exe"'
           ' -or $t.Actions[0].Arguments -cne '+quote(task_arguments(pc))+
           ' -or $t.State -eq "Running"){throw "Native task ownership or cleanup differs"};'
           'Unregister-ScheduledTask -TaskName '+quote(task)+' -Confirm:$false}', timeout=30)


def cancel(pc, *, reason='cancel'):
    """Cooperate first; loss of this supervisor independently expires the heartbeat."""
    if reason not in ('cancel', 'deadline'):
        raise ValueError('Invalid native cancellation reason')
    remote('[IO.File]::WriteAllText('+quote(pc+'/cancel')+',"'+reason+'")', timeout=15)
    deadline = time.monotonic()+75
    while time.monotonic() < deadline:
        state = remote('if(Test-Path -LiteralPath '+quote(pc+'/done.json')+')'
                       '{Get-Content -LiteralPath '+quote(pc+'/done.json')+' -Raw}', timeout=10).strip()
        if state:
            if len(state) > 128:
                raise ValueError('Native completion marker exceeds its bound')
            return json.loads(state)['exit']
        time.sleep(1)
    raise TimeoutError('Native worker cleanup is not yet confirmed; heartbeat expiry remains armed')


def run(args):
    root = Path(git(Path.cwd(), 'rev-parse', '--show-toplevel'))
    if git(root, 'rev-parse', 'HEAD') != args.commit or git(root, 'status', '--porcelain'):
        raise ValueError('Native qualification requires the selected clean source')
    commands = windows_gate.native_commands(args.package, workspace=args.workspace,
        nightly=args.nightly, jobs=args.jobs)
    if not commands:
        raise ValueError('Select Windows packages or the workspace')
    cache = root/'scripts/build-cache.sh'
    pins = re.findall(r'^export SECODE_FIXTURE_COMMIT=([0-9a-f]{40})$', cache.read_text(), re.M)
    if len(pins) != 1:
        raise ValueError('Native qualification requires one pinned fixture source')
    args.receipt = args.receipt.resolve()
    log = args.receipt.with_suffix(args.receipt.suffix+'.log')
    for path in (args.receipt, log):
        if path.exists() or path.is_symlink():
            raise ValueError('Preserve earlier native qualification output: '+str(path))
    args.receipt.parent.mkdir(parents=True, exist_ok=True)
    pc = os.environ['SECODE_WINDOWS_GATE_ROOT'].replace('\\', '/').rstrip('/')+'/runs/'+uuid.uuid4().hex
    task = 'SECode-NativeGate-'+pc.rsplit('/', 1)[1]
    deadline = time.monotonic()+args.timeout
    remote('New-Item -ItemType Directory -Path '+quote(pc)+'|Out-Null', timeout=30)
    launched = False
    done = False
    try:
        with tempfile.TemporaryDirectory(prefix='secode-native-stage-') as directory:
            stage = Path(directory)
            bundle = stage/'source.bundle'
            subprocess.run(['git', 'bundle', 'create', str(bundle), 'HEAD'], cwd=root,
                           check=True, timeout=120)
            raw = read(bundle, 256*1024**2)
            request = dict(commit=args.commit, packages=sorted(set(args.package)), workspace=args.workspace,
                nightly=args.nightly, jobs=args.jobs, commands=commands, fixture_commit=pins[0],
                bundle_sha256=hashlib.sha256(raw).hexdigest())
            helpers = Path(__file__).resolve().parent
            names = ('operator_paths.py', 'windows_native.ps1', 'windows_native_worker.py', 'windows_native_environment.py',
                     'windows_native_result.py', 'windows_native_survivors.py', 'windows_gate.py', 'test_features.py', 'slow_tests.py')
            policy = root/'scripts/build' if getattr(args, 'source_policy', None) else helpers
            files = [(policy if name in ('windows_gate.py', 'test_features.py', 'slow_tests.py') else helpers)/name
                     for name in names]
            for path in [bundle, *files,
                         version_worker()]:
                transfer(path, pc+'/'+path.name)
            request['timeout'] = max(1, int(deadline-time.monotonic()))
            (stage/'request.json').write_text(json.dumps(request)+'\n')
            transfer(stage/'request.json', pc+'/request.json')
            launched = True  # A lost SSH reply must still take the cancellation path.
            start(pc, task, request['timeout'])
            try:
                code = wait(pc, deadline)
            except TimeoutError:
                # Startup can consume the last of the controller budget. Join
                # the worker and collect its outcome instead of discarding a
                # capacity wait as an infrastructure or product test failure.
                code = cancel(pc, reason='deadline')
            done = True  # Published only after the owned Windows worker has exited.
            raw = collect(pc, 'native.log', log, MAX_LOG)
            result_path = stage/'result.json'
            result = json.loads(collect(pc, 'result.json', result_path, 65536))
            if code == CAPACITY_EXIT and result.get('exit') == CAPACITY_EXIT and result.get('status') == 'capacity':
                capacity_path = args.receipt.with_suffix(args.receipt.suffix+'.capacity.json')
                capacity_path.write_text(json.dumps(dict(pc_run=pc, result=result), indent=2)+'\n')
                raise CapacityUnavailable(str(result.get('error', 'Native PC capacity unavailable')))
            if code or result.get('exit') != 0:
                error_path = args.receipt.with_suffix(args.receipt.suffix+'.failed.json')
                error_path.write_text(json.dumps(dict(pc_run=pc, result=result), indent=2)+'\n')
                raise RuntimeError('Native Windows qualification failed: '+str(result.get('error', code)))
            if hashlib.sha256(raw).hexdigest() != result.get('log_sha256'):
                raise ValueError('Native log changed during collection')
            # A failure cannot leave a reusable receipt, even if a remote result
            # was present. Publish only after all source/scope/log checks pass.
            candidate = stage/'candidate.json'
            candidate.write_text(json.dumps(result, indent=2)+'\n')
            candidate.with_suffix('.json.log').write_bytes(raw)
            verify(candidate, commit=args.commit, packages=args.package,
                   workspace=args.workspace, nightly=args.nightly)
            with args.receipt.open('x') as output:
                output.write(candidate.read_text())
            print('PASS native Windows: '+args.commit+'; result '+str(args.receipt), flush=True)
    finally:
        if launched and not done:
            cancel(pc)
        if launched:
            # done.json precedes the wrapper's final exit by a few instructions.
            until = time.monotonic()+15
            while time.monotonic() < until:
                if remote('$t=Get-ScheduledTask -TaskName '+quote(task)+' -ErrorAction SilentlyContinue;'
                          'if(-not $t -or $t.State -ne "Running"){"stopped"}', timeout=10).strip() == 'stopped':
                    break
                time.sleep(0.2)
            remove_task(pc, task)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-policy', type=Path)
    parser.add_argument('--commit', required=True)
    parser.add_argument('--receipt', required=True, type=Path)
    parser.add_argument('--package', action='append', default=[])
    parser.add_argument('--workspace', action='store_true')
    parser.add_argument('--nightly', action='store_true')
    parser.add_argument('--jobs', type=int, choices=(2,), default=2)
    parser.add_argument('--timeout', type=int, default=10800)
    args = parser.parse_args()
    if not re.fullmatch('[0-9a-f]{40}', args.commit) or not 1 <= args.timeout <= 14400:
        parser.error('An exact commit and timeout in [1, 14400] are required')
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, interrupt)
    try:
        run(args)
    except CapacityUnavailable as error:
        print('CAPACITY native Windows: '+str(error), flush=True)
        raise SystemExit(CAPACITY_EXIT)


if __name__ == '__main__':
    main()
