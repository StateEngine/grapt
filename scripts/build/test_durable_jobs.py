"""A seat's cgroup can disappear without killing its admitted worker."""
from pathlib import Path
import shutil
import fcntl
import json
import os
import io
from types import SimpleNamespace
import runpy
import subprocess
import sys
import tempfile
import time
import unittest
import uuid
from unittest.mock import patch

import cpu_priority
import durable_jobs


class DurableJobsTests(unittest.TestCase):
    def test_explicit_archive_frees_full_history_and_preserves_terminal_follow(self):
        token='a'*32
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary);jobs=base/'jobs';jobs.mkdir(mode=0o700)
            path=jobs/token;path.mkdir(mode=0o700)
            request=dict(job=token,unit='secode-job-'+token+'.service',source='b'*40,
                         owner='completed-owner',log=str(path/'output.log'))
            result=dict(job=token,source=request['source'],owner=request['owner'],exit=7,
                        service_result='exit-code',finished_unix=time.time()-120)
            (path/'request.json').write_text(json.dumps(request))
            (path/'result.json').write_text(json.dumps(result))
            os.utime(path/'result.json',(time.time()-120,)*2)
            (path/'lifecycle.lock').touch(mode=0o600)
            (path/'output.log').write_bytes(b'original terminal output\n'+b'x'*65536)
            for index in range(255):(jobs/format(index,'032x')).mkdir(mode=0o700)
            with patch.object(durable_jobs,'JOBS',jobs):
                with self.assertRaisesRegex(ValueError,'history is full'):
                    durable_jobs.run(['worker'],'new-owner')
                with patch.object(durable_jobs,'service_state',return_value='active'):
                    with self.assertRaisesRegex(ValueError,'has not stopped'):durable_jobs.archive(token)
                with (path/'lifecycle.lock').open('rb') as held:
                    fcntl.flock(held,fcntl.LOCK_SH|fcntl.LOCK_NB)
                    with self.assertRaises(BlockingIOError):durable_jobs.archive(token)
                (path/'result.json').write_text(json.dumps(dict(result,source='c'*40)))
                os.utime(path/'result.json',(time.time()-120,)*2)
                with self.assertRaisesRegex(ValueError,'matching terminal'):durable_jobs.archive(token)
                (path/'result.json').write_text(json.dumps(result));os.utime(path/'result.json',(time.time()-120,)*2)
                original={name:(path/name).read_bytes() for name in ('request.json','result.json','output.log')}
                collision=base/'completed-jobs'/token;collision.mkdir(mode=0o700)
                with patch.object(durable_jobs,'service_state',return_value='inactive'):
                    with self.assertRaises(FileExistsError):durable_jobs.archive(token)
                    collision.rmdir()
                    streamed=[]
                    class RelocatingOutput:
                        def write(self,raw):
                            streamed.append(raw)
                            durable_jobs.archive(token)
                        def flush(self):pass
                    stdout=SimpleNamespace(buffer=RelocatingOutput(),write=lambda text:len(text),flush=lambda:None)
                    with patch.object(durable_jobs.sys,'stdout',stdout):
                        self.assertEqual(durable_jobs.follow(token),7)
                    self.assertEqual(b''.join(streamed),original['output.log'])
                    destination=durable_jobs.directory(token)
                    self.assertEqual(durable_jobs.archive(token),destination)
                self.assertFalse(path.exists())
                self.assertEqual(durable_jobs.directory(token),destination)
                self.assertEqual({name:(destination/name).read_bytes() for name in original},original)
                with patch.object(durable_jobs.sys,'stdout',SimpleNamespace(buffer=io.BytesIO(),write=lambda text:len(text),flush=lambda:None)):
                    self.assertEqual(durable_jobs.follow(token),7)
                with (patch.object(durable_jobs.subprocess,'check_output',return_value='b'*40),
                      patch.object(durable_jobs.subprocess,'run',return_value=subprocess.CompletedProcess([],0)),
                      patch.object(durable_jobs,'follow',return_value=0)):
                    self.assertEqual(durable_jobs.run(['worker'],'new-owner'),0)

    def test_scheduled_archive_preserves_current_receipts_and_warns_before_full(self):
        import durable_archive
        import install_schedules
        with tempfile.TemporaryDirectory() as temporary:
            base=Path(temporary); jobs=base/'jobs'; jobs.mkdir(mode=0o700)
            now=time.time(); current='a'*40; previous='b'*40
            tokens={name:format(index+1,'032x') for index,name in enumerate(
                ('expired','superseded','current','recent','success','pinned','locked','unreadable','active'))}
            for name,token in tokens.items():
                path=jobs/token; path.mkdir(mode=0o700)
                age=7*3600 if name in ('expired','pinned','locked','active') else 7200
                if name=='recent':age=120
                request=dict(job=token,unit='secode-job-'+token+'.service',owner='owner',
                             source=current if name=='current' else previous,cwd=str(base/name))
                (base/name).mkdir()
                (path/'request.json').write_text(json.dumps(request))
                result=dict(job=token,source=request['source'],owner='owner',
                            exit=0 if name in ('expired','success') else 1,finished_unix=now-age)
                (path/'result.json').write_text(json.dumps(result));os.utime(path/'result.json',(now-age,)*2)
                (path/'lifecycle.lock').touch(mode=0o600)
                if name=='pinned':(path/'retain-for-receipt').touch()
            def head(command,**options):
                return subprocess.CompletedProcess(command,1 if command[2].endswith('unreadable') else 0,current+'\n')
            def state(unit):return 'active' if tokens['active'] in unit else 'inactive'
            with (patch.object(durable_jobs,'JOBS',jobs),patch.object(durable_jobs,'service_state',side_effect=state),
                  patch.object(durable_archive.subprocess,'run',side_effect=head),
                  patch('nightly_revert.registry',return_value=([],'agt-test-coordinator')),
                  patch('nightly_revert.send') as notify,
                  (jobs/tokens['locked']/'lifecycle.lock').open('rb') as held):
                fcntl.flock(held,fcntl.LOCK_SH|fcntl.LOCK_NB)
                preview=durable_archive.sweep()
                self.assertTrue((jobs/tokens['expired']).exists())
                result=durable_archive.sweep(apply=True,actor='agt-test-actor')
                self.assertEqual(set(result['selected']),{tokens['expired'],tokens['superseded'],tokens['success']})
                for name,token in tokens.items():
                    self.assertEqual((jobs/token).exists(),name not in ('expired','superseded','success'))
                self.assertTrue(any('Unreadable checkout' in error for error in result['errors']))
                with self.assertRaisesRegex(ValueError,'retained for a pending receipt'):
                    durable_jobs.archive(tokens['pinned'])
                durable_archive.pressure(240,'agt-test-actor',now)
                durable_archive.pressure(241,'agt-test-actor',now+1)
                notify.assert_called_once()
                self.assertIn('16 slots free',notify.call_args.args[2])
            specifications=install_schedules.units(base,base,'agt-test-actor',base/'controller')
            self.assertIn('OnUnitInactiveSec=5min',specifications['secode-job-history.timer'])
            self.assertIn('--apply --agent-id agt-test-actor',specifications['secode-job-history.service'])
            self.assertIn('Environment=PATH=/usr/local/bin:/usr/bin:/bin',specifications['secode-job-history.service'])
            self.assertIn('EnvironmentFile=-',specifications['secode-job-history.service'])

    def test_owned_session_preserves_signal_status_and_explicit_143(self):
        controls = str(Path(durable_jobs.__file__).parent)
        payload = ("import sys;sys.path.insert(0,"+repr(controls)+");import heavy\n"
                   "for code in ('import os,signal;os.kill(os.getpid(),signal.SIGTERM)', 'raise SystemExit(143)'):\n"
                   " with heavy.owned_process([sys.executable,'-c',code]) as child:\n"
                   "  status=child.wait(timeout=10)\n"
                   " print(status,flush=True)\n")
        token = None
        original = durable_jobs.follow
        def follow(selected):
            nonlocal token
            token = selected
            return original(selected)
        try:
            with patch.object(durable_jobs, 'follow', side_effect=follow):
                self.assertEqual(durable_jobs.run([sys.executable,'-B','-c',payload],
                                                'job-signal-regression'),0)
            self.assertEqual((durable_jobs.directory(token)/'output.log').read_text().splitlines(),
                             ['-15','143'])
        finally:
            if token is not None:
                path = durable_jobs.directory(token)
                subprocess.run(['systemctl','--user','stop',durable_jobs.read(path/'request.json')['unit']],
                               capture_output=True,timeout=190)
                shutil.rmtree(path)

    def test_follow_and_cancel_refuse_a_copied_or_foreign_service_request(self):
        token = 'a'*32
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root/token
            path.mkdir(mode=0o700)
            helper = Path(durable_jobs.__file__)
            for row in ({'job':'b'*32,'unit':'secode-job-'+token+'.service'},
                        {'job':token,'unit':'unrelated-user.service'}):
                (path/'request.json').write_text(json.dumps(row))
                with patch.object(durable_jobs, 'JOBS', root), patch.object(durable_jobs, 'service_state') as state:
                    with self.assertRaisesRegex(ValueError,'does not own'):durable_jobs.follow(token)
                    state.assert_not_called()
                with patch.object(sys, 'argv', [str(helper),'--cancel',token]), \
                        patch.object(Path,'home',return_value=root), \
                        patch.object(durable_jobs.subprocess,'run') as execute:
                    selected=root/'.local/state/secode/jobs'/token
                    selected.mkdir(mode=0o700,parents=True,exist_ok=True)
                    (selected/'request.json').write_text(json.dumps(row))
                    with self.assertRaisesRegex(ValueError,'does not own'):runpy.run_path(str(helper),run_name='__main__')
                    execute.assert_not_called()

    @unittest.skipUnless(cpu_priority.configured(),
                          'requires the provisioned secodegate.slice (build box only)')
    def test_service_cancellation_joins_its_sibling_priority_slice_and_cannot_pass(self):
        token = None
        watcher = None
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            controls = str(Path(durable_jobs.__file__).parent)
            writer = ('import os,time;from pathlib import Path;Path('+repr(str(root/'writer'))+
                      ').write_text(str(os.getpid()));time.sleep(120)')
            payload = ('import sys;sys.path.insert(0,'+repr(controls)+');import cpu_priority;'
                       'raise SystemExit(cpu_priority.run('+repr([sys.executable,'-B','-c',writer])+',timeout=130))')
            caller = ('import sys;from pathlib import Path;sys.path.insert(0,'+repr(controls)+');import durable_jobs\n'
                      'original=durable_jobs.follow\n'
                      'def follow(token):\n Path('+repr(str(root/'token'))+').write_text(token)\n return original(token)\n'
                      'durable_jobs.follow=follow\n'
                      'raise SystemExit(durable_jobs.run('+repr([sys.executable,'-B','-c',payload])+',"job-cancellation-regression"))')
            def arrived(path):
                until = time.monotonic()+20
                while not path.exists() and time.monotonic()<until:time.sleep(.02)
                self.assertTrue(path.exists(), 'Missing cancellation observation: '+str(path))
            try:
                with (root/'watcher.log').open('w') as log:
                    watcher=subprocess.Popen([sys.executable,'-B','-c',caller],stdout=log,stderr=subprocess.STDOUT)
                arrived(root/'token');token=(root/'token').read_text();arrived(root/'writer')
                path=durable_jobs.directory(token);request=durable_jobs.read(path/'request.json')
                pid=(root/'writer').read_text()
                subprocess.run(['systemctl','--user','stop',request['unit']],check=True,capture_output=True,timeout=190)
                arrived(path/'result.json')
                self.assertNotEqual(durable_jobs.read(path/'result.json')['exit'],0)
                self.assertNotEqual(watcher.wait(timeout=10),0)
                try:
                    self.assertIn(Path('/proc',pid,'stat').read_text().rsplit(')',1)[1].split()[0],('Z','X'))
                except FileNotFoundError:
                    pass
            finally:
                if token is not None:
                    path=durable_jobs.directory(token)
                    subprocess.run(['systemctl','--user','stop',durable_jobs.read(path/'request.json')['unit']],
                                   capture_output=True,timeout=190)
                    import cpu_priority
                    for record in path.glob('priority-*.slice'):cpu_priority.stop(record.read_text().strip())
                    shutil.rmtree(path)
                if watcher is not None and watcher.poll() is None:
                    watcher.terminate();watcher.wait(timeout=10)

    def test_seat_service_stop_preserves_worker_and_its_nonzero_terminal_result(self):
        unit = 'secode-seat-test-'+uuid.uuid4().hex+'.service'
        token = None
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            payload = ('from pathlib import Path;import time;root=Path('+repr(str(root))+');'
                       '(root/"ready").touch();print("worker alive",flush=True)\n'
                       'while not (root/"release").exists():time.sleep(.02)\n'
                       'raise SystemExit(7)')
            caller = ('import sys;from pathlib import Path;sys.path.insert(0,'+
                      repr(str(Path(durable_jobs.__file__).parent))+');import durable_jobs\n'
                      'original=durable_jobs.follow\n'
                      'def follow(token):\n Path('+repr(str(root/'token'))+').write_text(token)\n return original(token)\n'
                      'durable_jobs.follow=follow\n'
                      'raise SystemExit(durable_jobs.run('+repr([sys.executable,'-B','-c',payload])+',"seat-relaunch-regression"))')
            def arrived(path):
                until = time.monotonic()+20
                while not path.exists() and time.monotonic()<until:time.sleep(.02)
                self.assertTrue(path.exists(), 'Missing independent job observation: '+str(path))
            try:
                subprocess.run(['systemd-run','--user','--quiet','--collect','--unit='+unit,
                    '--slice=app.slice','--working-directory='+str(Path.cwd()),
                    '-p','KillMode=control-group','-p','RuntimeMaxSec=60',
                    '--',sys.executable,'-B','-c',caller],check=True,capture_output=True,timeout=10)
                arrived(root/'token');token=(root/'token').read_text();arrived(root/'ready')
                path=durable_jobs.directory(token);request=durable_jobs.read(path/'request.json')
                arguments=['systemctl','--user','show',request['unit'],'-p','MainPID','--value']
                before=subprocess.check_output(arguments,text=True,timeout=5).strip()
                subprocess.run(['systemctl','--user','stop',unit],check=True,capture_output=True,timeout=10)
                self.assertEqual(subprocess.check_output(arguments,text=True,timeout=5).strip(),before)
                self.assertEqual(durable_jobs.service_state(request['unit']),'active')
                self.assertEqual(Path('/proc',before,'comm').read_text().strip(),'flock')
                (root/'release').touch();arrived(path/'result.json')
                row=durable_jobs.read(path/'result.json')
                self.assertEqual((row['exit'],row['owner'],row['source']),
                                 (7,'seat-relaunch-regression',request['source']))
                self.assertIn('worker alive',(path/'output.log').read_text())
                self.assertEqual(durable_jobs.follow(token),7)
            finally:
                subprocess.run(['systemctl','--user','stop',unit],capture_output=True,timeout=10)
                if token is not None:
                    path=durable_jobs.directory(token)
                    subprocess.run(['systemctl','--user','stop',durable_jobs.read(path/'request.json')['unit']],
                                   capture_output=True,timeout=190)
                    shutil.rmtree(path)


    def test_job_spawn_never_exports_lease_tags_to_the_service(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            captured = {}
            def spawn(arguments, **options):
                captured['arguments'] = arguments
                return subprocess.CompletedProcess(arguments, 0, stdout='', stderr='')
            environment = dict(os.environ,
                SECODE_HEAVY_LEASE=json.dumps({'fd': 9}),
                SECODE_FULL_GATE_LEASE=json.dumps({'fd': 10}),
                SECODE_RAM_TARGET_RUN=str(root))
            with patch.object(durable_jobs, 'JOBS', root / 'jobs'), \
                    patch.object(durable_jobs, 'filled', return_value=False), \
                    patch.object(durable_jobs.subprocess, 'run', side_effect=spawn), \
                    patch.object(durable_jobs.subprocess, 'check_output', return_value='f' * 40), \
                    patch.object(durable_jobs, 'follow', return_value=0), \
                    patch.dict(os.environ, environment):
                durable_jobs.run([sys.executable, '-B', '-c', 'pass'], 'lease-test')
            exported = {argument[len('--setenv='):].split('=', 1)[0]
                        for argument in captured['arguments']
                        if argument.startswith('--setenv=')}
            for tag in durable_jobs.NONTRANSFERABLE_ENV:
                self.assertNotIn(tag, exported, tag + ' crossed the job boundary')
            self.assertIn('SECODE_DURABLE_JOB', exported)

    def test_aborted_admission_leaves_no_history_slot(self):
        for fault in ('fail', 'bad-source'):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                def spawn(arguments, **options):
                    raise AssertionError('An aborted admission must not reach the service')
                def source(command, **options):
                    if fault == 'fail':
                        raise subprocess.CalledProcessError(128, command)
                    return 'not-a-commit\n'
                with patch.object(durable_jobs, 'JOBS', root/'jobs'), \
                        patch.object(durable_jobs, 'filled', return_value=False), \
                        patch.object(durable_jobs.subprocess, 'run', side_effect=spawn), \
                        patch.object(durable_jobs.subprocess, 'check_output', side_effect=source), \
                        patch.object(durable_jobs, 'follow', return_value=0):
                    command = [sys.executable, '-B', '-c', 'pass']
                    if fault == 'fail':
                        with self.assertRaises(subprocess.CalledProcessError):
                            durable_jobs.run(command, 'abort-regression')
                    else:
                        with self.assertRaisesRegex(ValueError, 'identifiable source'):
                            durable_jobs.run(command, 'abort-regression')
                leftovers = list((root/'jobs').iterdir()) if (root/'jobs').is_dir() else []
                self.assertEqual(leftovers, [])

class DurableFollowTimeoutTests(unittest.TestCase):
    def follow_fixture(self, root, query, advance):
        token = 'a'*32
        path = root/token
        path.mkdir(mode=0o700)
        (path/'request.json').write_text(json.dumps(dict(
            job=token, unit='secode-job-'+token+'.service', source='b'*40)))
        def sleep(_delay):
            advance[0] += 11
        with patch.object(durable_jobs, 'JOBS', root), \
                patch.object(durable_jobs.subprocess, 'run', side_effect=query), \
                patch.object(durable_jobs.time, 'monotonic', side_effect=lambda: advance[0]), \
                patch.object(durable_jobs.time, 'sleep', side_effect=sleep):
            return durable_jobs.follow(token)

    def test_timed_out_query_recovers_and_only_terminal_result_sets_exit(self):
        for code in (0, 7):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                calls = []
                def query(command, **options):
                    calls.append(command)
                    if len(calls) == 1:
                        raise subprocess.TimeoutExpired(command, options['timeout'])
                    if len(calls) == 3:
                        (root/('a'*32)/'result.json').write_text(json.dumps(
                            dict(job='a'*32, source='b'*40, exit=code)))
                    return subprocess.CompletedProcess(command, 0, stdout='active\n')
                self.assertEqual(self.follow_fixture(root, query, [100]), code)
                self.assertEqual(len(calls), 3)

    def test_unknown_state_expires_but_a_known_answer_resets_patience(self):
        for answered in (False, True):
            with self.subTest(answered=answered), tempfile.TemporaryDirectory() as temporary:
                now = [100]
                last_answer = [100]
                def query(command, **options):
                    if answered and last_answer[0] == 100 and now[0] >= 400:
                        last_answer[0] = now[0]
                        return subprocess.CompletedProcess(command, 0, stdout='active\n')
                    raise subprocess.TimeoutExpired(command, options['timeout'])
                with self.assertRaisesRegex(RuntimeError, 'service state is unavailable'):
                    self.follow_fixture(Path(temporary), query, now)
                self.assertGreater(now[0]-last_answer[0], durable_jobs.STATE_PATIENCE)
                self.assertLessEqual(now[0]-last_answer[0], durable_jobs.STATE_PATIENCE+11)


class DurableArchiveTests(unittest.TestCase):
    def records(self, root):
        jobs = root/'jobs'
        jobs.mkdir(mode=0o700)
        for token in ('a'*32, 'b'*32):
            path = jobs/token
            path.mkdir(mode=0o700)
            request = dict(job=token, unit='secode-job-'+token+'.service',
                           source='c'*40, log=str(path/'output.log'))
            (path/'request.json').write_text(json.dumps(request))
            (path/'result.json').write_text(json.dumps(dict(
                request, exit=7, finished_unix=time.time()-14400)))
        return jobs

    def test_archived_job_can_still_be_followed(self):
        with tempfile.TemporaryDirectory() as temporary:
            jobs = self.records(Path(temporary))
            with patch.object(durable_jobs, 'JOBS', jobs):
                self.assertEqual(durable_jobs.follow('a'*32), 7)
                durable_jobs.archive_completed(1)
                self.assertFalse((jobs/('a'*32)).exists())
                self.assertEqual(durable_jobs.follow('a'*32), 7)

    def test_archival_tolerates_a_candidate_moved_by_another_caller(self):
        with tempfile.TemporaryDirectory() as temporary:
            jobs = self.records(Path(temporary))
            original = durable_jobs.read
            moved = []
            def read(path):
                if not moved and path.name == 'result.json':
                    target = jobs.parent/'jobs-archive'/path.parent.name
                    path.parent.rename(target)
                    moved.append(target)
                return original(path)
            with patch.object(durable_jobs, 'JOBS', jobs), \
                    patch.object(durable_jobs, 'read', side_effect=read):
                durable_jobs.archive_completed(2)
            self.assertEqual(len(moved), 1)
            self.assertLess(len(list(jobs.iterdir())), 2)
            self.assertTrue((moved[0]/'result.json').is_file())

    def test_unrecognized_result_shapes_preserve_record_and_allow_cleanup(self):
        for outcome in ([], None, 7, 'finished'):
            with self.subTest(outcome=outcome), tempfile.TemporaryDirectory() as temporary:
                jobs = self.records(Path(temporary))
                unexpected = jobs/('a'*32)/'result.json'
                unexpected.write_text(json.dumps(outcome))
                with patch.object(durable_jobs, 'JOBS', jobs):
                    self.assertEqual(durable_jobs.archive_completed(2), 1)
                self.assertEqual(json.loads(unexpected.read_text()), outcome)
                self.assertTrue((jobs.parent/'jobs-archive'/('b'*32)/'result.json').is_file())

    def test_archive_sweeps_stale_empty_records_and_stranded_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            jobs = root/'jobs'
            jobs.mkdir(mode=0o700)
            old_empty = jobs/('a'*32); old_empty.mkdir(mode=0o700)
            young_empty = jobs/('b'*32); young_empty.mkdir(mode=0o700)
            old_busy = jobs/('c'*32); old_busy.mkdir(mode=0o700)
            (old_busy/'request.json').write_text('{}')
            old_stale = jobs/('staging-'+'d'*32); old_stale.mkdir(mode=0o700)
            (old_stale/'request.json').write_text('{}')
            young_stale = jobs/('staging-'+'e'*32); young_stale.mkdir(mode=0o700)
            day_ago = time.time() - 86400 - 3600
            for path in (old_empty, old_busy, old_stale):
                os.utime(path, (day_ago, day_ago))
            with patch.object(durable_jobs, 'JOBS', jobs):
                durable_jobs.archive_completed(durable_jobs.MAX_HISTORY)
            self.assertFalse(old_empty.exists(), 'stale empty record still holds a slot')
            self.assertTrue(young_empty.is_dir())
            self.assertTrue(old_busy.is_dir())
            self.assertFalse(old_stale.exists(), 'stranded staging record still holds a slot')
            self.assertTrue(young_stale.is_dir())


class DurableArchiveLogTests(unittest.TestCase):
    def test_moved_request_points_at_the_archived_log(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            jobs = root/'jobs'
            jobs.mkdir(mode=0o700)
            old, young = 'a'*32, 'b'*32
            for token, age in ((old, 14400), (young, 3600)):
                path = jobs/token
                path.mkdir(mode=0o700)
                (path/'request.json').write_text(json.dumps(dict(
                    job=token, unit='secode-job-'+token+'.service',
                    source='c'*40, log=str(path/'output.log'))))
                (path/'result.json').write_text(json.dumps(dict(
                    job=token, source='c'*40, exit=0,
                    finished_unix=time.time()-age)))
            with patch.object(durable_jobs, 'JOBS', jobs):
                self.assertEqual(durable_jobs.archive_completed(2), 1)
            self.assertEqual(durable_jobs.read(root/'jobs-archive'/old/'request.json')['log'],
                             str(root/'jobs-archive'/old/'output.log'))
            self.assertEqual(durable_jobs.read(jobs/young/'request.json')['log'],
                             str(jobs/young/'output.log'))
            self.assertFalse((root/'jobs-archive'/young).exists())


if __name__=='__main__':unittest.main()
