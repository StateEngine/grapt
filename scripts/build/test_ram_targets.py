import hashlib
import json
import os
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import ram_targets as ram


class RamTargetsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)/'ram'
        self.checkout = Path(self.temporary.name)/'checkout'
        self.checkout.mkdir()
        self.patch = patch.object(ram, 'ROOT', self.root)
        self.patch.start()
        self.addCleanup(self.patch.stop)
        self.free = patch.object(ram, 'available', return_value=128*ram.GIB)
        self.free.start()
        self.addCleanup(self.free.stop)
        self.fs = patch.object(ram.subprocess, 'check_output', return_value='tmpfs\n')
        self.fs.start()
        self.addCleanup(self.fs.stop)
        self.capacity = patch.object(ram.shutil, 'disk_usage', return_value=type('Usage', (), {'free':128*ram.GIB})())
        self.capacity.start()
        self.addCleanup(self.capacity.stop)

    def allocate(self):
        return ram.allocate(self.checkout)

    def test_three_reserved_runs_fill_hard_total_even_before_writing(self):
        first, a = self.allocate()
        second, b = self.allocate()
        self.assertNotEqual(first, second)
        third, c = self.allocate()
        self.assertNotIn(third, (first, second))
        self.assertEqual(a['budget']+b['budget']+c['budget'], ram.TOTAL_BYTES)
        with self.assertRaisesRegex(ValueError, 'reservation limit'):
            self.allocate()
        self.assertEqual(len(ram.entries()), 3)

    def test_memory_floor_and_tmpfs_space_refuse_without_waiting(self):
        with patch.object(ram, 'available', return_value=47*ram.GIB):
            with self.assertRaisesRegex(ValueError, 'available-memory'):
                self.allocate()
        with patch.object(ram.shutil, 'disk_usage', return_value=type('Usage', (), {'free':ram.GIB})()):
            with self.assertRaisesRegex(ValueError, 'tmpfs capacity'):
                self.allocate()
        self.assertEqual(ram.entries(), [])

    def test_unknown_content_and_symlink_root_are_preserved(self):
        self.root.mkdir(mode=0o700)
        unknown = self.root/'unknown'; unknown.mkdir()
        (unknown/'source').write_text('preserve')
        with self.assertRaises(ValueError):
            self.allocate()
        self.assertEqual(ram.reap(), [])
        self.assertEqual((unknown/'source').read_text(), 'preserve')
        self.root.rename(self.root.with_name('real'))
        self.root.symlink_to(self.root.with_name('real'), target_is_directory=True)
        with self.assertRaises(ValueError):
            self.allocate()

    def test_busy_admission_immediately_falls_back(self):
        with ram.locked():
            with self.assertRaises(BlockingIOError):
                self.allocate()

    def test_live_owner_preserved_and_dead_reused_pid_or_other_boot_reaped(self):
        path, row = self.allocate()
        self.assertEqual(ram.reap(), [])
        for mode in ('dead', 'reused', 'boot'):
            with self.subTest(mode=mode):
                if not path.exists():
                    path, row = self.allocate()
                actual = list(row['owner'])
                effect = FileNotFoundError() if mode == 'dead' else [actual[0], actual[1]+'1', actual[2]]
                if mode == 'boot':
                    effect = [actual[0], actual[1], '00000000-0000-0000-0000-000000000000']
                options = {'side_effect':effect} if isinstance(effect, Exception) else {'return_value':effect}
                def stop(command, **kwargs):
                    return subprocess.CompletedProcess(command, 0,
                        stdout='LoadState=loaded\nActiveState=inactive\nControlGroup=\n')
                original = subprocess.run
                with patch.object(ram, 'identity', **options), patch.object(ram.subprocess, 'run', side_effect=stop):
                    self.assertEqual(ram.reap(dry_run=True), [path])
                    self.assertTrue(path.exists())
                    # Exercise the real bounded deletion, while mocking only unit control.
                def control(command, **kwargs):
                    if command[0] == 'systemctl': return stop(command, **kwargs)
                    return original(command, **kwargs)
                with patch.object(ram, 'identity', **options), patch.object(ram.subprocess, 'run', side_effect=control):
                    self.assertEqual(ram.reap(), [path])
                self.assertFalse(path.exists())

    def test_reaper_preserves_target_if_writers_cannot_stop(self):
        path, _ = self.allocate()
        with patch.object(ram, 'identity', side_effect=ProcessLookupError()), patch.object(
                ram.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0,
                    stdout='LoadState=loaded\nActiveState=active\nControlGroup=\n')):
            with self.assertRaisesRegex(RuntimeError,'Orphan RAM targets remain'):
                ram.reap()
        self.assertTrue(path.exists())

    def test_only_live_governed_target_is_admitted(self):
        path, row = self.allocate()
        target = path/'target'/'worktrees'/hashlib.sha256(str(self.checkout).encode()).hexdigest()
        group = Path(self.temporary.name)/row['unit']; group.mkdir()
        for name, text in [('memory.max',str(ram.RUN_BYTES)),('memory.swap.max','0'),('memory.oom.group','1')]:
            (group/name).write_text(text)
        with patch.dict(os.environ, {'SECODE_RAM_TARGET_RUN':str(path)}), patch.object(ram,'cgroup',return_value=group):
            self.assertTrue(ram.validate(target,self.checkout))
            with self.assertRaises(ValueError): ram.validate(target/'foreign',self.checkout)
            for name, bad in [('memory.max',str(ram.RUN_BYTES+1)),('memory.swap.max','max'),('memory.oom.group','0')]:
                previous=(group/name).read_text(); (group/name).write_text(bad)
                with self.assertRaises(ValueError): ram.validate(target,self.checkout)
                (group/name).write_text(previous)
            with patch.object(ram, 'identity', return_value=[row['owner'][0],'reused',row['owner'][2]]):
                with self.assertRaises(ValueError): ram.validate(target,self.checkout)

    def test_fallback_is_recorded_and_leaves_original_disk_environment(self):
        import evict_targets
        log=Path(self.temporary.name)/'gate.log'
        disk=str(Path(self.temporary.name)/'disk')
        with patch.dict(os.environ, {'CARGO_TARGET_DIR':disk,'SECODE_TARGET_ROOT':disk},clear=True), \
             patch.object(evict_targets,'prepare') as prepare, \
             patch.object(ram,'allocate',side_effect=ValueError('memory floor')), \
             patch.object(ram,'pressure',return_value='some avg60=1.0'):
            self.assertIsNone(ram.run_gate(['affected','--log',str(log)],log))
            self.assertEqual(os.environ['CARGO_TARGET_DIR'],disk)
            prepare.return_value.close.assert_called_once()
        record=json.loads(log.with_suffix('.log.ram.json').read_text())
        self.assertEqual(record['mode'],'disk')
        self.assertEqual(record['fallback'],'memory floor')

    def test_borrowed_reservation_keeps_scratch_and_cleanup_with_scheduler(self):
        import evict_targets
        import heavy
        path, row = self.allocate()
        (path/'started').write_text('verified')
        (path/'accounting.json').write_text(json.dumps({'exit':0,'oom_kill':0}))
        log = Path(self.temporary.name)/'borrowed.log'
        with patch.dict(os.environ, {'CARGO_TARGET_DIR':'/disk','SECODE_TARGET_ROOT':'/disk'}, clear=True), \
                patch.object(evict_targets, 'prepare'), patch.object(heavy, 'reservation', return_value=(path,row)), \
                patch.object(heavy, 'inherited_fds', return_value=(41,42)), \
                patch.object(heavy, 'note_target'), patch.object(heavy, 'require_disk') as exclusive, \
                patch.object(ram, 'allocate') as allocate, patch.object(ram, 'remove') as remove, \
                patch.object(ram.subprocess, 'Popen') as launch:
            launch.return_value.wait.return_value = 0
            self.assertEqual(ram.run_gate(['affected','--log',str(log)], log), 0)
            allocate.assert_not_called()
            remove.assert_not_called()
            exclusive.assert_not_called()
            options = launch.call_args.kwargs
            self.assertEqual(options['pass_fds'], (41,42))
            for key in ('TMPDIR','TMP','TEMP'):
                self.assertEqual(options['env'][key], str(path/'tmp'))
            self.assertEqual((path/'tmp').stat().st_mode & 0o777, 0o700)
        self.assertTrue(path.exists())

    def test_borrowed_oom_joins_cleanup_before_exclusive_disk_readmission(self):
        import evict_targets
        import heavy
        path, row = self.allocate()
        (path/'started').write_text('verified')
        (path/'accounting.json').write_text(json.dumps({'exit':1,'oom_kill':1}))
        log = Path(self.temporary.name)/'oom.log'
        events = []
        with patch.dict(os.environ, {'CARGO_TARGET_DIR':'/disk','SECODE_TARGET_ROOT':'/disk'}, clear=True), \
                patch.object(evict_targets, 'prepare'), patch.object(heavy, 'reservation', return_value=(path,row)), \
                patch.object(heavy, 'inherited_fds', return_value=()), patch.object(heavy, 'note_target'), \
                patch.object(heavy, 'require_disk', side_effect=lambda target: events.append('exclusive')), \
                patch.object(ram, 'remove', side_effect=lambda *a: events.append('joined-and-removed')), \
                patch.object(ram.subprocess, 'Popen') as launch:
            launch.return_value.wait.return_value = 1
            self.assertIsNone(ram.run_gate(['affected','--log',str(log)], log))
        self.assertEqual(events[0], 'joined-and-removed')
        self.assertEqual(events[1:], ['exclusive'])

    def test_changed_boot_reaps_without_inspecting_a_reused_unreadable_pid(self):
        path,row=self.allocate()
        row['owner'][2]='00000000-0000-0000-0000-000000000000'
        (path/ram.MARKER).write_text(json.dumps(row))
        with patch.object(ram,'identity',side_effect=PermissionError()) as identity, patch.object(ram,'remove') as remove:
            self.assertEqual(ram.reap(),[path])
            identity.assert_not_called()
            self.assertEqual(remove.call_args.args[:2],(path,row))

    def test_expired_reaper_deadline_preserves_targets_without_starting_work(self):
        import time
        path,_=self.allocate()
        with patch.object(ram,'remove') as remove:
            with self.assertRaisesRegex(RuntimeError,'time limit'):
                ram.reap(deadline=time.monotonic()-1)
            remove.assert_not_called()
        self.assertTrue(path.exists())

    def test_control_failures_and_unavailable_state_never_invoke_deletion(self):
        path,row=self.allocate()
        inactive='LoadState=loaded\nActiveState=inactive\nControlGroup=\n'
        for stop_code,query_code,output in [(1,1,''),(0,1,''),(0,1,inactive),(1,0,inactive),(0,0,''),
                                           (0,0,'LoadState=not-found\n')]:
            with self.subTest(stop=stop_code,query=query_code,output=output):
                responses=[subprocess.CompletedProcess([],stop_code),
                           subprocess.CompletedProcess([],query_code,stdout=output)]
                with patch.object(ram.subprocess,'run',side_effect=responses) as run:
                    with self.assertRaises(RuntimeError):ram.remove(path,row)
                    self.assertEqual(run.call_count,2)
                self.assertTrue(path.exists())

    def test_positive_absence_or_stopped_unit_allows_cleanup(self):
        original=subprocess.run
        for stop_code,load in [(1,'not-found'),(0,'loaded')]:
            path,row=self.allocate()
            def control(command,**kwargs):
                if command[0]!='systemctl':return original(command,**kwargs)
                return subprocess.CompletedProcess(command,stop_code if 'stop' in command else 0,
                    stdout=f'LoadState={load}\nActiveState=inactive\nControlGroup=\n')
            with patch.object(ram.subprocess,'run',side_effect=control):ram.remove(path,row)
            self.assertFalse(path.exists())

    def test_populated_or_unreadable_stopped_cgroup_preserves_target(self):
        path,row=self.allocate()
        group=f'/user.slice/{row["unit"]}'
        output=f'LoadState=loaded\nActiveState=failed\nControlGroup={group}\n'
        original=Path.read_text
        for result in ['populated 1\nfrozen 0\n', PermissionError('hidden')]:
            def read(file,*args,**kwargs):
                if file.name=='cgroup.events':
                    if isinstance(result,Exception):raise result
                    return result
                return original(file,*args,**kwargs)
            with patch.object(ram.subprocess,'run',return_value=subprocess.CompletedProcess([],0,stdout=output)) as run, \
                 patch.object(Path,'read_text',read):
                with self.assertRaises((RuntimeError,PermissionError)):ram.remove(path,row)
                self.assertEqual(run.call_count,2)
            self.assertTrue(path.exists())

    def test_only_own_empty_failed_scope_is_reset_after_target_removal(self):
        path,row=self.allocate()
        original=subprocess.run
        resets=[]
        def control(command,**kwargs):
            if command[0]!='systemctl':return original(command,**kwargs)
            if 'reset-failed' in command:
                self.assertFalse(path.exists())
                resets.append(command)
            return subprocess.CompletedProcess(command,0,
                stdout='LoadState=loaded\nActiveState=failed\nControlGroup=\n')
        with patch.object(ram.subprocess,'run',side_effect=control):ram.remove(path,row)
        self.assertEqual(resets,[['systemctl','--user','reset-failed',row['unit']]])

    def test_failed_scope_reset_failure_is_reported_after_safe_target_cleanup(self):
        path,row=self.allocate()
        original=subprocess.run
        def control(command,**kwargs):
            if command[0]!='systemctl':return original(command,**kwargs)
            if 'reset-failed' in command:
                raise subprocess.CalledProcessError(1,command)
            return subprocess.CompletedProcess(command,0,
                stdout='LoadState=loaded\nActiveState=failed\nControlGroup=\n')
        with patch.object(ram.subprocess,'run',side_effect=control):
            with self.assertRaises(subprocess.CalledProcessError):ram.remove(path,row)
        self.assertFalse(path.exists())

    def test_worker_failure_propagates_but_oom_or_missing_accounting_falls_back(self):
        import evict_targets
        for accounting in [None, {'exit':1,'oom_kill':1}, {'exit':7,'oom_kill':0}, {'exit':75,'oom_kill':0}]:
            with self.subTest(accounting=accounting):
                path,row=self.allocate()
                (path/'started').write_text('verified')
                if accounting is not None:(path/'accounting.json').write_text(json.dumps(accounting))
                log=Path(self.temporary.name)/'worker.log'
                with patch.dict(os.environ, {'CARGO_TARGET_DIR':'/disk','SECODE_TARGET_ROOT':'/disk'},clear=True), \
                     patch.object(evict_targets,'prepare'),patch.object(ram,'allocate',return_value=(path,row)), \
                     patch.object(ram.subprocess,'Popen') as launch,patch.object(ram,'remove') as remove:
                    expected = accounting['exit'] if accounting and not accounting['oom_kill'] else None
                    launch.return_value.wait.return_value = expected if expected is not None else 7
                    code=ram.run_gate(['affected','--log='+str(log)],log)
                    self.assertEqual(code,expected)
                    remove.assert_called_once_with(path,row)
                record=json.loads(log.with_suffix('.log.ram.json').read_text())
                self.assertEqual(record['mode'],'ram' if code is not None else 'disk')
                self.assertEqual(record['outcome'], 'disk-admission-refused' if code == 75 else
                                 'failed' if code == 7 else 'fallback')
                import shutil
                shutil.rmtree(path)

    def test_failed_and_oom_runs_preserve_hashes_before_target_cleanup(self):
        import evict_targets
        import heavy
        for code, oom in [(0, 0), (7, 0), (1, 1)]:
            with self.subTest(code=code, oom=oom):
                path, row = self.allocate()
                (path/'started').write_text('verified')
                (path/'accounting.json').write_text(json.dumps({'exit':code, 'oom_kill':oom}))
                target = path/'target/worktrees'/hashlib.sha256(str(Path.cwd()).encode()).hexdigest()
                deps = target/'debug/deps'
                deps.mkdir(parents=True)
                (deps/'libreqwest-123.rlib').write_bytes(b'failed dependency')
                log = Path(self.temporary.name)/f'failed-{code}-{oom}.log'
                sidecar = log.with_suffix('.log.ram.deps.json')
                def cleanup(*_args):
                    if code:
                        saved = json.loads(sidecar.read_text())
                        self.assertEqual(saved['files'][0]['sha256'],
                                         hashlib.sha256(b'failed dependency').hexdigest())
                    else:
                        self.assertFalse(sidecar.exists())
                    shutil.rmtree(path)
                with patch.dict(os.environ, {'CARGO_TARGET_DIR':'/disk','SECODE_TARGET_ROOT':'/disk'}, clear=True), \
                        patch.object(evict_targets, 'prepare'), \
                        patch.object(ram, 'allocate', return_value=(path,row)), \
                        patch.object(ram, 'remove', side_effect=cleanup), \
                        patch.object(heavy, 'require_exclusive'), \
                        patch.object(ram.subprocess, 'Popen') as launch:
                    launch.return_value.wait.return_value = code
                    self.assertEqual(ram.run_gate(['full', '--log', str(log)], log), None if oom else code)
                if code:
                    self.assertTrue(sidecar.is_file(), 'Cleanup removed the failure evidence')

    def test_failed_ram_gate_archives_its_real_log_and_completed_sidecars(self):
        import evict_targets
        import heavy
        path, row = self.allocate()
        (path / 'started').write_text('verified')
        (path / 'accounting.json').write_text(json.dumps({'exit': 7, 'oom_kill': 0}))
        target = path / 'target/worktrees' / hashlib.sha256(str(Path.cwd()).encode()).hexdigest()
        deps = target / 'debug/deps'
        deps.mkdir(parents=True)
        (deps / 'libfixture.rmeta').write_bytes(b'failed dependency')
        log = Path(self.temporary.name) / 'failed.log'
        log.write_bytes(b'RAM gate failure')
        log.with_suffix('.log.nextest.xml').write_bytes(b'<testsuites/>')
        archive = Path(self.temporary.name) / 'archive'
        environment = {'CARGO_TARGET_DIR': '/disk', 'SECODE_TARGET_ROOT': '/disk',
                       'SECODE_GATE_FAILURE_ARCHIVE': str(archive)}
        with patch.dict(os.environ, environment, clear=True), \
                patch.object(evict_targets, 'prepare'), \
                patch.object(heavy, 'reservation', return_value=None), \
                patch.object(ram, 'allocate', return_value=(path, row)), \
                patch.object(ram, 'remove'), patch.object(heavy, 'note_target'), \
                patch.object(ram.subprocess, 'Popen') as launch:
            launch.return_value.wait.return_value = 7
            self.assertEqual(ram.run_gate(['full', '--log', str(log)], log), 7)
        groups = list(archive.iterdir()) if archive.exists() else []
        self.assertEqual(len(groups), 1, 'failed RAM gate has no retained archive')
        saved = groups[0]
        self.assertEqual((saved / log.name).read_bytes(), log.read_bytes())
        self.assertEqual((saved / (log.name + '.nextest.xml')).read_bytes(), b'<testsuites/>')
        self.assertEqual(json.loads((saved / (log.name + '.ram.json')).read_text())['outcome'], 'failed')
        inventory = json.loads((saved / (log.name + '.ram.deps.json')).read_text())
        self.assertEqual(inventory['files'][0]['sha256'], hashlib.sha256(b'failed dependency').hexdigest())

    def test_interrupted_ram_attempt_is_not_recorded_as_disk_fallback(self):
        import evict_targets
        path,row=self.allocate()
        (path/'started').write_text('verified')
        log=Path(self.temporary.name)/'interrupted.log'
        with patch.dict(os.environ, {'CARGO_TARGET_DIR':'/disk','SECODE_TARGET_ROOT':'/disk'},clear=True), \
             patch.object(evict_targets,'prepare'),patch.object(ram,'allocate',return_value=(path,row)), \
             patch.object(ram.subprocess,'Popen') as launch,patch.object(ram,'remove') as remove:
            launch.return_value.wait.side_effect=[KeyboardInterrupt(),-15]
            with self.assertRaises(KeyboardInterrupt):
                ram.run_gate(['full','--log',str(log)],log)
            remove.assert_called_once_with(path,row)
        record=json.loads(log.with_suffix('.log.ram.json').read_text())
        self.assertEqual(record['mode'],'ram')
        self.assertEqual(record['outcome'],'interrupted')
        self.assertEqual(record['exit'],130)
        self.assertIsNone(record['fallback'])


    def test_cancellation_survives_cleanup_and_child_reap_failures(self):
        import evict_targets
        path,row=self.allocate()
        for cleanup_fails, reap_fails in [(True,False),(False,True),(True,True)]:
            with self.subTest(cleanup=cleanup_fails,reap=reap_fails):
                log=Path(self.temporary.name)/'cancel-errors.log'
                cleanup_error=RuntimeError('writer stop unavailable')
                reap_error=subprocess.TimeoutExpired('systemd-run',10)
                # A second cleanup would succeed: it must not turn cancellation into fallback.
                removal=[cleanup_error,None] if cleanup_fails else [None,None]
                with patch.dict(os.environ, {'CARGO_TARGET_DIR':'/disk','SECODE_TARGET_ROOT':'/disk'},clear=True), \
                     patch.object(evict_targets,'prepare'),patch.object(ram,'allocate',return_value=(path,row)), \
                     patch.object(ram.subprocess,'Popen') as launch, \
                     patch.object(ram,'remove',side_effect=removal) as remove:
                    launch.return_value.wait.side_effect=[KeyboardInterrupt(),reap_error if reap_fails else -15]
                    with self.assertRaises(KeyboardInterrupt):
                        ram.run_gate(['full','--log',str(log)],log)
                    remove.assert_called_once_with(path,row)
                    timeout=launch.return_value.wait.call_args.kwargs['timeout']
                    self.assertGreaterEqual(timeout,10)
                    self.assertLessEqual(timeout,240)
                record=json.loads(log.with_suffix('.log.ram.json').read_text())
                self.assertEqual((record['mode'],record['outcome'],record['exit']),('ram','interrupted',130))
                self.assertIsNone(record['fallback'])
                self.assertEqual('cleanup_error' in record,cleanup_fails)
                self.assertEqual('child_reap_error' in record,reap_fails)
                if cleanup_fails:
                    self.assertEqual(record['cleanup_error'],str(cleanup_error))
                    self.assertTrue(path.exists())

    def test_interrupted_reap_scales_to_a_hard_cap_without_disk_readmission(self):
        import evict_targets
        import heavy
        path, row = self.allocate()
        for load, expected in [(0, 30.0), (4, 120.0), (100, 240.0)]:
            with self.subTest(load=load):
                log = Path(self.temporary.name)/'bounded-reap.log'
                with patch.dict(os.environ, CARGO_TARGET_DIR='/disk',
                                SECODE_TARGET_ROOT='/disk', clear=True), \
                        patch.object(evict_targets, 'prepare'), \
                        patch.object(ram, 'allocate', return_value=(path, row)), \
                        patch.object(ram.os, 'getloadavg', return_value=(load, 0, 0)), \
                        patch.object(ram.os, 'cpu_count', return_value=1), \
                        patch.object(ram.subprocess, 'Popen') as launch, \
                        patch.object(ram, 'remove') as remove, \
                        patch.object(heavy, 'require_disk') as readmit:
                    launch.return_value.wait.side_effect = [KeyboardInterrupt(), -15]
                    with self.assertRaises(KeyboardInterrupt):
                        ram.run_gate(['full', '--log', str(log)], log)
                    self.assertEqual(launch.return_value.wait.call_args.kwargs,
                                     {'timeout': expected})
                    remove.assert_called_once_with(path, row)
                    readmit.assert_not_called()
                record = json.loads(log.with_suffix('.log.ram.json').read_text())
                self.assertEqual((record['outcome'], record['exit']), ('interrupted', 130))
                self.assertIsNone(record['fallback'])


if __name__ == '__main__':
    unittest.main()
