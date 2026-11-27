"""Native command coverage and the gate's exact-source result boundary."""
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
from types import SimpleNamespace
from contextlib import contextmanager
import shutil

import windows_gate
from windows_native_result import SCHEMA, verify
import windows_native_worker as native
from windows_native_result import CapacityUnavailable


class NativeWindowsTests(unittest.TestCase):
    def test_native_transport_uses_its_reviewed_cleanup_sidecar(self):
        import windows_native as controller
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);helpers=root/'private';helpers.mkdir()
            selected=helpers/'windows_version_worker.py';selected.write_text('reviewed cleanup')
            with patch.object(controller,'__file__',str(helpers/'windows_native.py')):
                self.assertEqual(controller.version_worker(),selected)
                selected.unlink()
                with self.assertRaisesRegex(ValueError,'missing'):
                    controller.version_worker()

    def test_cleanup_joins_an_exiting_child_that_denies_another_termination(self):
        import importlib.util
        path = Path(__file__).resolve().parent.parent/'release/windows_version_worker.py'
        spec = importlib.util.spec_from_file_location('native_cleanup_regression', path)
        job = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(job)
        class Function:
            def __init__(self, call): self.call = call
            def __call__(self, *args): return self.call(*args)
        joined = [False]
        def membership(_handle, _job, member):
            member._obj.value = True
            return True
        def wait(_handle, timeout):
            if timeout > 0: joined[0] = True
            return 0 if joined[0] else 258
        kernel = SimpleNamespace(**{name: Function(call) for name, call in {
            'OpenProcess': lambda *_: 123, 'IsProcessInJob': membership,
            'TerminateProcess': lambda *_: False, 'WaitForSingleObject': wait,
            'CloseHandle': lambda *_: True}.items()})
        with patch.object(job.C, 'WinDLL', return_value=kernel, create=True), \
                patch.object(job.C, 'get_last_error', return_value=5, create=True), \
                patch.object(job.C, 'WinError', side_effect=lambda error: OSError(error, 'termination denied'), create=True), \
                patch.object(job, 'process_ids', side_effect=lambda: [os.getpid()] + ([] if joined[0] else [999])):
            job.stop_descendants(timeout=1)
        self.assertTrue(joined[0])

    def test_only_signed_msvc_vctip_is_joined_before_native_success(self):
        import io
        import importlib.util
        source=Path(__file__).resolve().parent.parent/'release/windows_version_worker.py'
        spec=importlib.util.spec_from_file_location('windows_version_worker',source)
        job=importlib.util.module_from_spec(spec);spec.loader.exec_module(job)
        self.enterContext(patch.dict(sys.modules,windows_version_worker=job))
        import windows_native_survivors as survivors
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);toolchain=root/'MSVC'/'14.44';(toolchain/'bin/HostX64/x64').mkdir(parents=True)
            approved=toolchain/'bin/HostX64/x64/vctip.exe';approved.touch()
            elsewhere=root/'vctip.exe';elsewhere.touch()
            other=toolchain/'bin/HostX64/x64/compiler.exe';other.touch()
            class Function:
                def __init__(self, call): self.call=call
                def __call__(self,*args): return self.call(*args)
            for paths,signed,allowed,denied in (([approved],True,True,False),([elsewhere],True,False,False),
                                         ([other],True,False,False),([approved],False,False,False),
                                         ([approved,other],True,False,False),([approved],True,True,True)):
                with self.subTest(paths=paths,signed=signed):
                    live={900+index:path for index,path in enumerate(paths)};terminated=[]
                    def membership(_handle,_job,member): member._obj.value=True;return True
                    def image(handle,_flags,buffer,_size): buffer.value=str(live[handle]);return True
                    def terminate(handle,_code): terminated.append(handle);del live[handle];return not denied
                    kernel=SimpleNamespace(**{name:Function(call) for name,call in {
                        'OpenProcess':lambda _access,_inherit,pid:pid,'IsProcessInJob':membership,
                        'QueryFullProcessImageNameW':image,'WaitForSingleObject':lambda handle,_wait:258 if handle in live else 0,
                        'TerminateProcess':terminate,'CloseHandle':lambda *_:True}.items()})
                    worker=SimpleNamespace(environment={'VCTOOLSINSTALLDIR':str(toolchain)},
                                           check=lambda:None,deadline=time.monotonic()+30,log=io.BytesIO())
                    with patch.object(survivors.C,'WinDLL',return_value=kernel,create=True), \
                            patch.object(survivors.job,'process_ids',side_effect=lambda:[os.getpid(),*live]), \
                            patch.object(survivors.job,'children_clean',side_effect=lambda:not live), \
                            patch.object(survivors,'microsoft_signed',return_value=signed), \
                            patch.object(survivors.C,'get_last_error',return_value=5,create=True):
                        if allowed:
                            actions=survivors.join_telemetry(worker)
                            self.assertEqual(actions,[dict(pid=900,path=str(approved.resolve()),action='joined_exit_pending' if denied else 'terminated')])
                            self.assertFalse(live)
                        else:
                            with self.assertRaisesRegex(RuntimeError,'unapproved owned descendant'):
                                survivors.join_telemetry(worker)
                            self.assertFalse(terminated)

    def test_native_environment_supplies_python3_from_its_existing_interpreter(self):
        import windows_native_environment as environment
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);run=root/'run';run.mkdir();checkout=root/'checkout';checkout.mkdir()
            installation=root/'visual-studio'
            vcvars=installation/'VC/Auxiliary/Build/vcvars64.bat'
            vcvars.parent.mkdir(parents=True);vcvars.touch()
            pin='b'*40;fixtures=root/'fixtures'/pin;fixtures.mkdir(parents=True)
            (fixtures/'.source-commit').write_text(pin+'\n')
            worker=SimpleNamespace(directory=run,request=dict(jobs=2,fixture_commit=pin),
                                   environment={'PATH':'old-tools'})
            def execute(command,**_options):
                if command[1:3]==['-m','venv']:
                    self.assertEqual(command[0],sys.executable)
                    scripts=Path(command[-1])/'Scripts';scripts.mkdir(parents=True)
                    (scripts/'python.exe').write_bytes(b'existing interpreter redirector')
                if command[0]=='cargo':
                    python=Path(worker.environment['PATH'].split(os.pathsep)[0])/'python3.exe'
                    self.assertEqual(python.read_bytes(),b'existing interpreter redirector')
                    self.assertTrue(worker.environment['PATH'].endswith(os.pathsep+'msvc-tools'))
                    self.assertNotIn('Path',worker.environment)
            worker.run=execute
            outputs=iter([str(installation).encode(),b'Path=msvc-tools\n'])
            def bootstrap(_command,**options):
                self.assertEqual(options['env']['VSCMD_SKIP_SENDTELEMETRY'],'1')
                return next(outputs)
            with patch.object(environment.subprocess,'CREATE_NO_WINDOW',0,create=True), \
                    patch.object(environment.subprocess,'check_output',side_effect=bootstrap):
                environment.prepare(worker,root,checkout)

    def test_nightly_yields_to_live_seats_but_seats_and_completed_runs_proceed(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);own=root/'runs'/('a'*32);own.mkdir(parents=True)
            (own/'request.json').write_text(json.dumps(dict(commit='a'*40,fixture_commit='b'*40,
                                                           timeout=10,nightly=True)))
            seat=own.parent/('b'*32);seat.mkdir()
            (seat/'request.json').write_text(json.dumps(dict(nightly=False)))
            (seat/'heartbeat').touch()
            worker=native.Worker(own)
            try:
                with patch.object(native,'ROOT',root), patch.object(native,'LEGACY_RUNS',root/'legacy'):
                    with self.assertRaises(CapacityUnavailable):worker.yield_to_seats()
                    worker.request['nightly']=False
                    worker.yield_to_seats()
                    worker.request['nightly']=True
                    (seat/'done.json').write_text('{"exit":0}')
                    worker.yield_to_seats()
            finally:
                worker.log.close()

    def test_nightly_inventory_tolerates_retirement_but_propagates_unreadable_entries(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);own=root/'runs'/('a'*32);own.mkdir(parents=True)
            (own/'request.json').write_text(json.dumps(dict(commit='a'*40,fixture_commit='b'*40,timeout=10,nightly=True)))
            candidate=own.parent/('b'*32);candidate.mkdir()
            scan=os.scandir;unreadable=[False];retire=[True]
            class RetiringEntry:
                name=candidate.name;path=str(candidate)
                def is_dir(self,**_options):return True
                def stat(self,**_options):
                    if unreadable[0]:raise PermissionError('unreadable native request')
                    if retire[0]:shutil.rmtree(candidate)
                    return candidate.lstat()
            @contextmanager
            def entries(path):
                with scan(path) as found:
                    yield (RetiringEntry() if entry.name==candidate.name else entry for entry in found)
            worker=native.Worker(own)
            try:
                with patch.object(native,'ROOT',root), patch.object(native,'LEGACY_RUNS',root/'legacy'), \
                        patch.object(native.os,'scandir',side_effect=entries):
                    worker.yield_to_seats()
                    candidate.mkdir();retire[0]=False
                    (candidate/'heartbeat').touch()
                    request=candidate/'request.json';request.write_text('{"nightly":false}')
                    read=native.read
                    def retired_request(path,maximum):
                        if path==request:path.unlink()
                        return read(path,maximum)
                    with patch.object(native,'read',side_effect=retired_request):
                        worker.yield_to_seats()
                    for content in ('{',' '*65537):
                        request.write_text(content)
                        with self.assertRaises(ValueError):worker.yield_to_seats()
                    unreadable[0]=True
                    with self.assertRaises(PermissionError):worker.yield_to_seats()
            finally:
                worker.log.close()

    def test_delayed_start_collects_capacity_after_the_controller_deadline(self):
        import windows_native as controller
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);pc=root/'pc';pc.mkdir()
            (root/'scripts').mkdir()
            (root/'scripts/build-cache.sh').write_text('export SECODE_FIXTURE_COMMIT='+'b'*40+'\n')
            (pc/'heartbeat').touch()
            (pc/'request.json').write_text(json.dumps(dict(commit='a'*40,fixture_commit='b'*40,timeout=10)))
            worker=native.Worker(pc);clock=[0.0];budget=[None]
            args=SimpleNamespace(commit='a'*40,package=['grapt-types'],workspace=False,
                                 nightly=False,jobs=2,timeout=5,receipt=root/'qualification.json')
            def git(_root,*command):
                return str(root) if command[-1]=='--show-toplevel' else ('' if command[0]=='status' else args.commit)
            def bundle(command,**_options):
                Path(command[3]).write_bytes(b'bundle')
            def transfer(path,_destination):
                if path.name=='request.json':
                    budget[0]=json.loads(path.read_text())['timeout']
                clock[0]=4.0
            def start(*_args):
                clock[0]=6.0  # The remote worker starts after the controller's deadline.
                worker.deadline=clock[0]+budget[0]
            def remote(command,**_options):
                if '/cancel' in command:
                    (pc/'cancel').write_text('deadline' if '"deadline"' in command else 'cancel')
                    with patch.object(native,'available_memory',return_value=1):
                        with self.assertRaises(CapacityUnavailable):worker.wait_capacity()
                    (pc/'result.json').write_text(json.dumps(dict(schema=SCHEMA,exit=75,status='capacity',error='PC memory floor')))
                return '{"exit":75}' if '/done.json' in command else 'stopped'
            def collect(_pc,name,destination,maximum):
                raw=(pc/name).read_bytes();self.assertLessEqual(len(raw),maximum)
                destination.write_bytes(raw);return raw
            try:
                with patch.dict(os.environ, SECODE_WINDOWS_GATE_ROOT=str(root/'native')), \
                        patch.object(controller,'git',side_effect=git), \
                        patch.object(controller.subprocess,'run',side_effect=bundle), \
                        patch.object(controller,'transfer',side_effect=transfer), \
                        patch.object(controller,'start',side_effect=start), \
                        patch.object(controller,'remote',side_effect=remote), \
                        patch.object(controller,'collect',side_effect=collect), \
                        patch.object(controller,'remove_task') as remove, \
                        patch.object(controller.time,'monotonic',side_effect=lambda:clock[0]):
                    with self.assertRaises(CapacityUnavailable):controller.run(args)
                remove.assert_called_once()
                self.assertTrue(args.receipt.with_suffix('.json.capacity.json').exists())
                self.assertFalse(args.receipt.exists())
                self.assertFalse(args.receipt.with_suffix('.json.failed.json').exists())
            finally:
                worker.log.close()

    @unittest.skipUnless(os.name == 'nt', 'requires the native Windows lease and job APIs')
    def test_cancellation_joins_live_child_before_a_second_waiter_acquires_the_lease(self):
        def arrived(path, rejected=None):
            until = time.monotonic()+10
            while not path.exists() and time.monotonic() < until:
                if rejected is not None:
                    self.assertFalse(rejected.exists(), 'checkout waiter overlapped a live owned child')
                time.sleep(0.02)
            self.assertTrue(path.exists(), 'missing native cancellation observation: '+path.name)
        with tempfile.TemporaryDirectory(prefix='secode-native-lease-', dir=os.environ['TEMP']) as directory:
            root = Path(directory)
            commands = windows_gate.native_commands(['grapt-types'])
            (root/'request.json').write_text(json.dumps(dict(commit='a'*40, fixture_commit='b'*40,
                timeout=60, packages=['grapt-types'], workspace=False, nightly=False, jobs=2,
                commands=commands, bundle_sha256=hashlib.sha256(b'fixture').hexdigest())))
            (root/'source.bundle').write_bytes(b'fixture')
            (root/'heartbeat').touch()
            children = []
            try:
                worker = subprocess.Popen([sys.executable, __file__, '--lease-worker', str(root)])
                children.append(worker)
                arrived(root/'child-ready')
                (root/'cancel').touch()
                arrived(root/'cleanup-started')
                waiters = [subprocess.Popen([sys.executable, __file__, mode, str(root)])
                           for mode in ('--lease-waiter', '--retention-waiter')]
                children.extend(waiters)
                for prefix in ('waiter', 'retention'):
                    arrived(root/(prefix+'-blocked'), root/'overlap')
                    self.assertFalse((root/(prefix+'-acquired')).exists())
                (root/'finish-cleanup').touch()
                self.assertNotEqual(worker.wait(timeout=10), 0)
                for waiter in waiters:
                    self.assertEqual(waiter.wait(timeout=10), 0)
                for prefix in ('waiter', 'retention'):
                    arrived(root/(prefix+'-acquired'))
                self.assertFalse((root/'overlap').exists(), 'checkout waiter overlapped a live owned child')
            finally:
                (root/'finish-cleanup').touch()
                (root/'cancel').touch()
                for process in children:
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill(); process.wait(timeout=5)

    def test_native_leg_runs_both_feature_configurations_with_a_bounded_job_count(self):
        commands = windows_gate.native_commands(['grapt-product'])
        tests = [c for c in commands if c[1] == 'test']
        self.assertEqual(len(tests), 2)
        self.assertIn('--features', tests[0])
        self.assertNotIn('--features', tests[1])
        for command in commands:
            self.assertNotIn('xwin', command)
            self.assertNotIn('--no-run', command)
            self.assertEqual(command[command.index('-j')+1], '2')
            self.assertIn('--locked', command)
        for count in (0, 1, 3, 4, 5, 2.0):
            with self.assertRaises(ValueError):
                windows_gate.native_commands(['grapt-types'], jobs=count)

    def test_low_memory_waits_before_work_and_expires_as_capacity(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'request.json').write_text(json.dumps(dict(commit='a'*40, fixture_commit='b'*40, timeout=10)))
            (root/'heartbeat').touch()
            worker = native.Worker(root)
            try:
                with patch.object(native, 'available_memory', side_effect=[1, 2_000_000_000]), \
                        patch.object(native.time, 'sleep') as pause:
                    worker.wait_capacity()
                    pause.assert_called_once()
                worker.deadline = time.monotonic()-1
                with patch.object(native, 'available_memory', return_value=1):
                    with self.assertRaises(CapacityUnavailable):
                        worker.wait_capacity()
                    (root/'cancel').touch()
                    with self.assertRaises(TimeoutError):
                        worker.wait_capacity()
                with patch.object(native.os, 'name', 'nt'), \
                        patch.object(native, 'Worker', return_value=worker), \
                        patch.object(worker, 'execute', side_effect=CapacityUnavailable('PC memory floor')):
                    self.assertEqual(native.main(root), 75)
                result = json.loads((root/'result.json').read_text())
                self.assertEqual((result['status'], result['exit']), ('capacity', 75))
            finally:
                worker.log.close()

    def test_join_refuses_wrong_source_scope_missing_ordinary_tests_and_changed_log(self):
        commit = 'a'*40
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'native.json'
            log = path.with_suffix('.json.log')
            log.write_text('native commands completed\n')
            valid = dict(schema=SCHEMA, exit=0, source_before=dict(commit=commit, clean=True),
                source_after=dict(commit=commit, clean=True), target=windows_gate.TARGET,
                packages=['grapt-product'], workspace=False, nightly=False, jobs=2,
                commands=windows_gate.native_commands(['grapt-product']), children_clean=True,
                finished_unix=time.time(), log_sha256=hashlib.sha256(log.read_bytes()).hexdigest())
            path.write_text(json.dumps(valid))
            verify(path, commit=commit, packages=['grapt-product'])
            for change in ({'source_after':dict(commit='b'*40, clean=True)},
                           {'packages':['grapt-types']}, {'commands':valid['commands'][:-2]},
                           {'log_sha256':'0'*64}, {'children_clean':False}, {'exit':1}):
                row = copy.deepcopy(valid); row.update(change)
                path.write_text(json.dumps(row))
                with self.assertRaises(ValueError):
                    verify(path, commit=commit, packages=['grapt-product'])


def lease_fixture(mode, root):
    """Real Windows processes, with a controlled cleanup pause for the race."""
    import ctypes as C
    from ctypes import wintypes as W
    import msvcrt
    if mode == '--live-child':
        (root/'child-ready').write_text(str(os.getpid()))
        time.sleep(30)
        return 0
    if mode == '--lease-worker':
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent/'release'))
        import windows_native_environment
        import windows_native_worker as native
        native.PC_LOCK, native.ROOT = root/'pc.lock', root/'checkout-root'
        native.available_memory = lambda: 2_000_000_000
        windows_native_environment.prepare = lambda *_: None
        class FixtureWorker(native.Worker):
            def pause_cleanup(self):
                (root/'cleanup-started').touch()
                until = time.monotonic()+10
                while not (root/'finish-cleanup').exists() and time.monotonic() < until:
                    time.sleep(0.02)
            def execute(self):
                try:
                    return super().execute()
                except BaseException:
                    # Expand the old gap between lease unwinding and error
                    # reporting. A corrected worker has already paused inside
                    # its lease and joined its child before it reaches here.
                    if not (root/'cleanup-started').exists():
                        self.pause_cleanup()
                    raise
            def source(self, _checkout):
                return dict(commit='a'*40, clean=True)
            def run(self, command, **_options):
                if command[0] != 'cargo':
                    return
                subprocess.Popen([sys.executable, __file__, '--live-child', str(root)])
                while True:
                    self.check(); time.sleep(0.02)
            def cleanup(self):
                self.pause_cleanup()
                super().cleanup()
        native.Worker = FixtureWorker
        return native.main(root)
    until = time.monotonic()+15
    prefix = 'waiter' if mode == '--lease-waiter' else 'retention'
    path = root/'pc.lock' if mode == '--lease-waiter' else root/'checkout-root/.retention.lock'
    with path.open('a+b') as lock:
        if lock.tell() == 0:lock.write(b'0');lock.flush()
        while time.monotonic() < until:
            try:
                lock.seek(0); msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
                break
            except OSError:
                (root/(prefix+'-blocked')).touch(); time.sleep(0.02)
        else:
            return 2
        kernel = C.WinDLL('kernel32', use_last_error=True)
        kernel.OpenProcess.restype = W.HANDLE
        kernel.OpenProcess.argtypes = [W.DWORD, W.BOOL, W.DWORD]
        kernel.WaitForSingleObject.argtypes = [W.HANDLE, W.DWORD]
        kernel.CloseHandle.argtypes = [W.HANDLE]
        process = kernel.OpenProcess(0x100000, False, int((root/'child-ready').read_text()))
        if process:
            try:
                if kernel.WaitForSingleObject(process, 0) != 0:
                    (root/'overlap').touch()
                    return 1
            finally:
                kernel.CloseHandle(process)
        (root/(prefix+'-acquired')).touch()
        return 0


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] in ('--lease-worker', '--lease-waiter', '--retention-waiter', '--live-child'):
        raise SystemExit(lease_fixture(sys.argv[1], Path(sys.argv[2])))
    unittest.main()
