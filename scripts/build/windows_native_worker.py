"""Windows-side gate worker; its job owns every compiler and test descendant."""
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time
import traceback
import operator_paths

import windows_gate
from windows_native_result import SCHEMA, MAX_LOG, CAPACITY_EXIT, CapacityUnavailable, read

ROOT = Path(__file__).resolve().parents[2]
# Share the PC controller lock used by the existing native package operators.
PC_LOCK = Path('E:/temp/kappa-perf-bench02-20261024/.pc-run.lock')
LEGACY_RUNS = Path('E:/temp/secode-native-gate')
MIN_FREE_MEMORY = 2_000_000_000


def available_memory():
    """Read physical availability without starting a WMI process on the PC."""
    import ctypes as C
    from ctypes import wintypes as W
    class MemoryStatus(C.Structure):
        _fields_ = [('length', W.DWORD), ('load', W.DWORD),
                    *[(name, C.c_ulonglong) for name in
                      ('total', 'available', 'page_total', 'page_available',
                       'virtual_total', 'virtual_available', 'extended_available')]]
    status = MemoryStatus()
    status.length = C.sizeof(status)
    kernel = C.WinDLL('kernel32', use_last_error=True)
    kernel.GlobalMemoryStatusEx.argtypes = [C.POINTER(MemoryStatus)]
    kernel.GlobalMemoryStatusEx.restype = W.BOOL
    if not kernel.GlobalMemoryStatusEx(C.byref(status)):
        raise C.WinError(C.get_last_error())
    return status.available


class Worker:
    def __init__(self, directory):
        self.directory = directory
        self.request = json.loads(read(directory/'request.json', 65536))
        if (not re.fullmatch('[0-9a-f]{40}', self.request['commit'])
                or not re.fullmatch('[0-9a-f]{40}', self.request['fixture_commit'])
                or type(self.request['timeout']) is not int or not 1 <= self.request['timeout'] <= 14400):
            raise ValueError('Native request requires exact commits and a bounded deadline')
        self.deadline = time.monotonic()+self.request['timeout']
        self.heartbeat = None
        self.seen = time.monotonic()
        self.environment = dict(os.environ)
        self.log = (directory/'native.log').open('xb')

    def check(self, *, capacity=False):
        stamp = (self.directory/'heartbeat').stat().st_mtime_ns
        if stamp != self.heartbeat:
            self.heartbeat, self.seen = stamp, time.monotonic()
        cancellation = self.directory/'cancel'
        if cancellation.exists():
            if capacity and read(cancellation, 64) == b'deadline':
                raise CapacityUnavailable('Native PC has less than 2 GB free physical memory')
            raise TimeoutError('Native gate cancelled')
        if time.monotonic()-self.seen > 60:
            raise TimeoutError('Native gate cancelled or controller lost')
        if time.monotonic() >= self.deadline:
            if capacity:
                raise CapacityUnavailable('Native PC has less than 2 GB free physical memory')
            raise TimeoutError('Native gate deadline exceeded')
        if self.log.tell() > MAX_LOG:
            raise ValueError('Native gate log exceeds 64 MiB')

    def run(self, command, *, cwd=None, stdout=None):
        if command[0] == 'cargo':
            self.yield_to_seats()
            self.wait_capacity()
        self.check()
        self.log.write(('+ '+subprocess.list2cmdline([str(v) for v in command])+'\n').encode())
        self.log.flush()
        process = subprocess.Popen(command, cwd=cwd, env=self.environment,
            stdin=subprocess.DEVNULL, stdout=stdout or self.log, stderr=self.log,
            creationflags=subprocess.CREATE_NO_WINDOW)
        while process.poll() is None:
            self.check()
            time.sleep(0.2)
        if process.returncode:
            raise subprocess.CalledProcessError(process.returncode, command)

    def wait_capacity(self):
        waiting = False
        while available_memory() < MIN_FREE_MEMORY:
            self.check(capacity=True)
            if not waiting:
                self.log.write(b'Waiting for native PC capacity: at least 2 GB free physical memory\n')
                self.log.flush()
                waiting = True
            time.sleep(0.2)
        self.check()

    def yield_to_seats(self):
        """Nightly gives up the PC at command boundaries when seats are waiting."""
        if not self.request['nightly']:
            return
        now = time.time()
        # The second root holds older versioned controllers still in flight.
        for root in (ROOT/'runs', LEGACY_RUNS):
            if not root.exists():
                continue
            info = root.lstat()
            if not stat.S_ISDIR(info.st_mode) or getattr(info, 'st_file_attributes', 0)&0x400:
                raise ValueError('Preserve a linked native run namespace')
            with os.scandir(root) as entries:
                for index, entry in enumerate(entries):
                    if index >= 16384:
                        raise ValueError('Native priority inventory exceeds its bound')
                    if not re.fullmatch('[0-9a-f]{32}', entry.name):
                        continue
                    candidate = Path(entry.path)
                    try:
                        if candidate == self.directory or not entry.is_dir(follow_symlinks=False):
                            continue
                        if getattr(entry.stat(follow_symlinks=False), 'st_file_attributes', 0)&0x400:
                            continue
                        if (candidate/'done.json').exists():
                            continue
                        heartbeat = (candidate/'heartbeat').lstat()
                        if (not stat.S_ISREG(heartbeat.st_mode) or getattr(heartbeat, 'st_file_attributes', 0)&0x400
                                or not -5 <= now-heartbeat.st_mtime <= 60):
                            continue
                        path = candidate/'request.json'
                        try:
                            request = json.loads(read(path, 65536))
                        except ValueError:
                            path.lstat()  # Only actual disappearance is benign.
                            raise
                    except FileNotFoundError:
                        continue  # Staging or completed retention is not a live requester.
                    if request.get('nightly') is False:
                        raise CapacityUnavailable('Nightly native work deferred behind a waiting seat')

    def source(self, checkout):
        def git(*args):
            return subprocess.check_output(['git', *args], cwd=checkout,
                creationflags=subprocess.CREATE_NO_WINDOW, timeout=30).decode().strip()
        return dict(commit=git('rev-parse', 'HEAD'), clean=not git('status', '--porcelain'))

    def execute(self):
        import msvcrt
        from windows_version_worker import contain
        contain(64)
        request = self.request
        commands = windows_gate.native_commands(request['packages'], workspace=request['workspace'],
                                                nightly=request['nightly'], jobs=request['jobs'])
        if not commands or commands != request['commands']:
            raise ValueError('Native request differs from its Windows coverage plan')
        self.yield_to_seats()
        # Keep compatibility with existing PC operators while retention uses
        # only its own namespace. New runs hold both leases through child join.
        ROOT.mkdir(exist_ok=True)
        paths = (PC_LOCK, ROOT/'.retention.lock')
        with ExitStack() as held:
            for path in paths:
                path.parent.mkdir(parents=True, exist_ok=True)
                lock = held.enter_context(path.open('a+b'))
                if lock.tell() == 0:
                    lock.write(b'0'); lock.flush()
                while True:
                    self.check()
                    try:
                        lock.seek(0)
                        msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
                        break
                    except OSError as error:
                        if error.errno not in (13, 36):
                            raise
                        time.sleep(0.2)
            active = [path.with_suffix('.active') for path in paths]
            for marker in active:
                if marker.exists():
                    raise RuntimeError('Prior native PC cleanup is unconfirmed: '+marker.read_text()[:4096])
            for marker in active:
                marker.write_text(str(self.directory)+'\n')
            try:
                self.yield_to_seats()
                self.wait_capacity()
                return self.qualify(commands)
            finally:
                self.cleanup()
                for marker in active:
                    marker.unlink()  # Only successful joining releases either writer fence.

    def cleanup(self):
        from windows_version_worker import stop_descendants
        stop_descendants()

    def qualify(self, commands):
        import windows_native_environment
        from windows_version_worker import children_clean, process_ids
        request = self.request
        ROOT.mkdir(exist_ok=True)
        checkout = ROOT/'checkout'
        if not checkout.exists():
            self.run(['git', 'init', '-q', str(checkout)])
        elif not self.source(checkout)['clean']:
            raise ValueError('Preserve dirty native gate checkout')
        self.run(['git', '-C', str(checkout), 'config', 'core.autocrlf', 'false'])
        self.run(['git', '-C', str(checkout), 'config', 'core.longpaths', 'true'])
        bundle = self.directory/'source.bundle'
        if hashlib.sha256(read(bundle, 256*1024**2)).hexdigest() != request['bundle_sha256']:
            raise ValueError('Native source bundle changed during transfer')
        self.run(['git', '-C', str(checkout), 'fetch', str(bundle), 'HEAD'])
        self.run(['git', '-C', str(checkout), 'checkout', '--detach', request['commit']])
        before = self.source(checkout)
        if before != dict(commit=request['commit'], clean=True):
            raise ValueError('Native checkout differs from selected clean source')
        windows_native_environment.prepare(self, ROOT, checkout)
        for command in commands:
            self.run(command, cwd=checkout)
        after = self.source(checkout)
        if after != before:
            raise ValueError('Native source changed while qualifying')
        until = time.monotonic()+5
        while not children_clean() and time.monotonic() < until:
            self.check(); time.sleep(0.1)
        telemetry_cleanup = []
        if not children_clean():
            from windows_native_survivors import join_telemetry
            telemetry_cleanup = join_telemetry(self)
        if not children_clean():
            pids = [pid for pid in process_ids() if pid != os.getpid()]
            script = ('$ids=@('+','.join(str(pid) for pid in pids)+');Get-CimInstance Win32_Process|'
                      'Where-Object {$ids -contains $_.ProcessId}|Select-Object ProcessId,Name,ExecutablePath|ConvertTo-Json -Compress')
            try:
                names = subprocess.check_output(['powershell.exe', '-NoProfile', '-NonInteractive', '-Command', script],
                    creationflags=subprocess.CREATE_NO_WINDOW, timeout=10)
                self.log.write(b'Native owned leftovers: '+names[:65536]+b'\n')
                self.log.flush()
            except (OSError, subprocess.SubprocessError) as error:
                self.log.write(('Native leftover images unavailable: '+str(error)[:512]+'\n').encode())
            raise RuntimeError('Native tests left owned descendants running: '+str(pids))
        self.log.flush()
        return dict(schema=SCHEMA, exit=0, source_before=before, source_after=after,
            target=windows_gate.TARGET, packages=request['packages'], workspace=request['workspace'],
            nightly=request['nightly'], jobs=request['jobs'], commands=commands, children_clean=True, telemetry_cleanup=telemetry_cleanup,
            finished_unix=time.time(), log_sha256=hashlib.sha256(read(self.directory/'native.log', MAX_LOG)).hexdigest())


def main(directory):
    if os.name != 'nt':
        raise RuntimeError('Native Windows gate worker requires Windows')
    worker = Worker(directory)
    code = 1
    try:
        result = worker.execute()
        code = 0
    except CapacityUnavailable as error:
        code = CAPACITY_EXIT
        result = dict(schema=SCHEMA, exit=code, status='capacity', error=str(error))
    except BaseException as error:
        traceback.print_exc(file=sys.stderr)
        result = dict(schema=SCHEMA, exit=1, error=str(error)[:4096])
    finally:
        worker.log.close()
    (directory/'result.json').write_text(json.dumps(result, indent=2)+'\n')
    return code


if __name__ == '__main__':
    raise SystemExit(main(Path(sys.argv[1]).resolve()))
