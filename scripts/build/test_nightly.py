"""Exercise culprit selection against real disposable Git histories."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import nightly
import install_schedules


class NightlyTests(unittest.TestCase):
    def setUp(self):
        from unittest.mock import patch
        # Only the capacity suite owns UTC-window behavior. Diagnosis and
        # signal tests must run unchanged during the measurement hour too.
        self.enterContext(patch.object(nightly.gate, 'nightly_start_allowed'))
        self.temp=tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.root=Path(self.temp.name)/'repository'; self.root.mkdir()
        self.state=Path(self.temp.name)/'state'
        subprocess.run(['git','init','-q',str(self.root)],check=True)
        self.history=[]

    def commit(self, succeeds, label):
        (self.root/'test.py').write_text(f'raise SystemExit({0 if succeeds else 1})\n')
        name=f'change-{len(self.history)}'
        (self.root/name).write_text(label)
        if not self.history:
            (self.root/'label').write_text('baseline')
        paths=['test.py',name]+(['label'] if not self.history else [])
        subprocess.run(['git','add','--',*paths],cwd=self.root,check=True)
        subprocess.run(['git','-c','user.name=Test','-c','user.email=test@localhost',
                        'commit','-qm',label,'--',*paths],cwd=self.root,check=True)
        commit=nightly.git(self.root,'rev-parse','HEAD'); self.history.append(commit)
        return commit

    def check(self, root, _state, _commit):
        return subprocess.run([sys.executable,'test.py'],cwd=root,timeout=5).returncode==0

    def test_green_updates_baseline_and_returns_to_current_dev(self):
        candidate=self.commit(True,'green')
        self.assertEqual(nightly.audit(self.root,self.state,candidate,self.check,dry_run=False),0)
        self.assertEqual(nightly.git(self.root,'rev-parse',nightly.GOOD_REF),candidate)
        self.assertEqual(nightly.git(self.root,'rev-parse','HEAD'),candidate)

    def test_red_names_the_actual_culprit_not_the_latest_commit(self):
        good=self.commit(True,'baseline')
        nightly.git(self.root,'update-ref',nightly.GOOD_REF,good)
        self.commit(True,'still good'); culprit=self.commit(False,'introduced defect')
        candidate=self.commit(False,'unrelated followup')
        self.assertEqual(nightly.audit(self.root,self.state,candidate,self.check),1)
        result=(self.state/'last-result').read_text()
        self.assertIn('RED culprit '+culprit,result)
        self.assertIn('Owner:',result)
        self.assertIn('dry run, canonical main unchanged',result)
        self.assertEqual(nightly.git(self.root,'rev-parse','HEAD'),candidate)
        self.assertEqual(nightly.git(self.root,'rev-parse',nightly.GOOD_REF),good)

    def test_failed_baseline_is_inconclusive_and_does_not_blame_a_commit(self):
        good=self.commit(True,'baseline')
        nightly.git(self.root,'update-ref',nightly.GOOD_REF,good)
        candidate=self.commit(False,'latest')
        self.assertEqual(nightly.audit(self.root,self.state,candidate,lambda *_:False),2)
        self.assertNotIn('RED culprit',(self.state/'last-result').read_text())
        self.assertEqual(nightly.git(self.root,'rev-parse','HEAD'),candidate)

    def test_transient_candidate_failure_does_not_name_an_innocent_commit(self):
        good=self.commit(True,'baseline')
        nightly.git(self.root,'update-ref',nightly.GOOD_REF,good)
        candidate=self.commit(True,'still green')
        attempts=0
        def transient(root,state,commit):
            nonlocal attempts
            if commit==candidate:
                attempts+=1
                if attempts==1:return False
            return self.check(root,state,commit)
        self.assertEqual(nightly.audit(self.root,self.state,candidate,transient),2)
        result=(self.state/'last-result').read_text()
        self.assertIn('candidate failure did not repeat',result)
        self.assertNotIn('RED culprit',result)
        self.assertEqual(nightly.git(self.root,'rev-parse','HEAD'),candidate)

    def test_first_nightly_requires_an_explicit_last_green_baseline(self):
        self.commit(True,'parent'); culprit=self.commit(False,'bad')
        self.assertEqual(nightly.audit(self.root,self.state,culprit,self.check),2)
        self.assertIn('no last green nightly',(self.state/'last-result').read_text())

    def test_changed_history_does_not_attribute_a_culprit(self):
        good=self.commit(True,'baseline')
        nightly.git(self.root,'update-ref',nightly.GOOD_REF,good)
        # Same exact source was previously green: a new failure is not a new commit.
        self.assertEqual(nightly.audit(self.root,self.state,good,lambda *_:False),2)
        self.assertNotIn('RED culprit',(self.state/'last-result').read_text())

    def test_bisect_preserves_candidate_checkout_if_a_runner_raises(self):
        good=self.commit(True,'baseline'); nightly.git(self.root,'update-ref',nightly.GOOD_REF,good)
        candidate=self.commit(False,'bad')
        def broken(*_):raise OSError('runner unavailable')
        with self.assertRaises(OSError):nightly.audit(self.root,self.state,candidate,broken)
        self.assertEqual(nightly.git(self.root,'rev-parse','HEAD'),candidate)
        self.assertIn('INCONCLUSIVE',(self.state/'last-result').read_text())

    def test_bisect_has_a_commit_bound_and_does_not_invent_a_culprit(self):
        from unittest.mock import patch
        good=self.commit(True,'baseline'); nightly.git(self.root,'update-ref',nightly.GOOD_REF,good)
        self.commit(True,'one'); self.commit(False,'two'); candidate=self.commit(False,'three')
        with patch.object(nightly,'MAX_COMMITS',2):
            self.assertEqual(nightly.audit(self.root,self.state,candidate,self.check),2)
        self.assertNotIn('RED culprit',(self.state/'last-result').read_text())

    def test_scheduler_refuses_to_take_over_an_existing_unmarked_checkout(self):
        from unittest.mock import patch
        with patch.object(sys,'argv',['install_schedules.py','--repository',str(self.root)]):
            with self.assertRaisesRegex(RuntimeError,'not marked for nightly'):
                install_schedules.main()

    def test_nightly_preserves_dirty_dedicated_checkout_before_any_fetch(self):
        from unittest.mock import patch
        self.commit(True,'baseline'); (self.root/'.git/secode-nightly-owner').write_text('owner')
        (self.root/'label').write_text('private work')
        with patch.object(sys,'argv',['nightly.py','--repository',str(self.root)]):
            with self.assertRaisesRegex(RuntimeError,'marked, clean clone'):
                nightly.main()
        self.assertEqual((self.root/'label').read_text(),'private work')

    def test_installed_supervisor_survives_broken_subject_code(self):
        library=Path(self.temp.name)/'installed'
        source=Path(__file__).resolve().parents[2]
        install_schedules.install_control(source,library)
        # The source checkout may disappear or contain a syntax error while
        # bisecting. Installed control imports only its deployed modules.
        (self.root/'nightly.py').write_text('this is deliberately invalid Python!')
        result=subprocess.run([sys.executable,str(library/'nightly.py'),'--help'],cwd=self.root,
                              capture_output=True,text=True,timeout=10)
        self.assertEqual(result.returncode,0,result.stderr)
        launcher=subprocess.run([str(library/'heavy'),'--help'],cwd=self.root,
                                capture_output=True,text=True,timeout=10)
        self.assertEqual(launcher.returncode,0,launcher.stderr)
        self.assertIn('--class',launcher.stdout)
        installed_gate=subprocess.run([sys.executable,str(library/'gate.py'),'--help'],
            cwd=self.root,text=True,capture_output=True,timeout=10)
        self.assertEqual(installed_gate.returncode,0,installed_gate.stderr)

    def test_controller_upgrade_installs_exit_observation_before_its_consumer(self):
        from unittest.mock import patch
        library=Path(self.temp.name)/'installed'; library.mkdir()
        (library/'cargo_observation.py').write_text('# Previous version lacks exited().\n')
        (library/'gate.py').write_text('# Previous version lacks guarded nightly capacity.\n')
        source=Path(__file__).resolve().parents[2]
        copy=install_schedules.atomic_copyfile
        checked=[]
        def install(path,destination):
            result=copy(path,destination)
            if Path(path).name=='evict_targets.py':
                probe=subprocess.run([sys.executable,'-I','-B','-c',
                    'import sys;sys.path.insert(0,sys.argv[1]);import evict_targets as t;'
                    'from pathlib import Path;assert t.cargo_observation.exited(Path("missing-fixture-pid"))',
                    str(library)],cwd=library,capture_output=True,text=True,timeout=10)
                self.assertEqual(probe.returncode,0,probe.stderr)
                checked.append('evict_targets')
            if Path(path).name=='nightly.py':
                # Import the deployed supervisor in a fresh process at the
                # exact consumer-copy transition, not after installation ends.
                probe=subprocess.run([sys.executable,'-I','-B','-c',
                    'import sys;sys.path.insert(0,sys.argv[1]);import nightly;'
                    'assert callable(nightly.gate.nightly_start_allowed);'
                    'assert issubclass(nightly.gate.GateSkipped,RuntimeError)',
                    str(library)],cwd=library,capture_output=True,text=True,timeout=10)
                self.assertEqual(probe.returncode,0,probe.stderr)
                checked.append('nightly')
            return result
        with patch.object(install_schedules,'atomic_copyfile',side_effect=install):
            install_schedules.install_control(source,library)
        self.assertEqual(checked,['evict_targets','nightly'])


    def test_initial_known_green_seed_must_belong_to_dev_history(self):
        good=self.commit(True,'baseline'); self.commit(True,'candidate')
        install_schedules.seed_green(self.root,good)
        self.assertEqual(nightly.git(self.root,'rev-parse',nightly.GOOD_REF),good)
        with self.assertRaises(ValueError):install_schedules.seed_green(self.root,'not-a-commit')
        with self.assertRaises(subprocess.CalledProcessError):
            install_schedules.seed_green(self.root,'f'*40)
        self.assertEqual(nightly.git(self.root,'rev-parse',nightly.GOOD_REF),good)

    def test_launcher_only_install_does_not_fetch_or_change_timers(self):
        from unittest.mock import patch
        home=Path(self.temp.name)/'launcher-home'
        with patch.object(sys,'argv',['install_schedules.py','--launcher-only']), \
                patch.object(Path,'home',return_value=home), \
                patch.object(install_schedules.subprocess,'run',side_effect=AssertionError('installation attempted a subprocess')):
            install_schedules.main()
        self.assertTrue((home/'.local/lib/secode-release/heavy').is_file())
        self.assertFalse((home/'.config/systemd').exists())
        legacy=(home/'.local/lib/secode-release/heavy').read_bytes()
        snapshot=home/'.local/lib/secode-release-snapshots'/('a'*40)
        with patch.object(sys,'argv',['install_schedules.py','--launcher-only','--control-directory',str(snapshot)]), \
                patch.object(Path,'home',return_value=home), \
                patch.object(install_schedules.subprocess,'run',side_effect=AssertionError('installation attempted a subprocess')):
            install_schedules.main()
            with self.assertRaisesRegex(ValueError,'fresh version'):
                install_schedules.main()
        self.assertEqual((home/'.local/lib/secode-release/heavy').read_bytes(),legacy)
        self.assertFalse(snapshot.stat().st_mode & 0o222)
        self.assertTrue((snapshot/'release_job.py').is_file())

    def test_interrupted_gate_does_not_become_a_red_source_result(self):
        import textwrap
        (self.root/'scripts').mkdir()
        (self.root/'scripts/gate.sh').write_text('#!/bin/sh\nexec sleep 30\n')
        (self.root/'scripts/build').mkdir()
        (self.root/'scripts/build/gate.py').write_text('FULL_CAPACITY_PROTOCOL = 1\n')
        self.state.mkdir()
        code = textwrap.dedent("""\
            import os,signal,sys,threading
            from pathlib import Path
            import nightly,heavy,full_capacity,heavy_sampling
            from fixture_environment import hardware_sample
            heavy_sampling.host=hardware_sample
            heavy.HEAVY_LOCK=Path(sys.argv[1])/'heavy'
            heavy.QUEUE_PATH=Path(sys.argv[1])/'queue'
            full_capacity.LOCK=Path(sys.argv[1])/'full.lock'
            heavy.nvme_target=lambda _:None
            nightly.gate.nightly_start_allowed=lambda:None
            signal.signal(signal.SIGTERM,nightly.request_stop)
            threading.Timer(0.2,lambda:os.kill(os.getpid(),signal.SIGTERM)).start()
            try:
                nightly.full_gate(Path(sys.argv[1]),Path(sys.argv[2]),'f'*40)
            except KeyboardInterrupt:
                raise SystemExit(42)
            raise SystemExit(1)
        """)
        environment=__import__('os').environ.copy()
        environment['PYTHONPATH']=str(Path(__file__).resolve().parent)
        result=subprocess.run([sys.executable,'-c',code,str(self.root),str(self.state)],
                              env=environment,capture_output=True,text=True,timeout=15)
        self.assertEqual(result.returncode,42,result.stderr)
        self.assertNotIn('Full gate failed',result.stdout)

    def test_an_earlier_fixed_regression_is_not_blamed_for_a_different_current_failure(self):
        script='from pathlib import Path\nraise SystemExit(int(Path("a").read_text()) or int(Path("b").read_text()))\n'
        def snapshot(label,a,b):
            (self.root/'test.py').write_text(script)
            (self.root/'a').write_text(str(a)); (self.root/'b').write_text(str(b))
            name=f'change-{len(self.history)}'; (self.root/name).write_text(label)
            paths=['test.py','a','b',name]
            subprocess.run(['git','add','--',*paths],cwd=self.root,check=True)
            subprocess.run(['git','-c','user.name=Test','-c','user.email=test@localhost',
                            'commit','-qm',label,'--',*paths],cwd=self.root,check=True)
            commit=nightly.git(self.root,'rev-parse','HEAD'); self.history.append(commit)
            return commit
        good=snapshot('baseline',0,0); nightly.git(self.root,'update-ref',nightly.GOOD_REF,good)
        snapshot('old defect',1,0); snapshot('unrelated change',1,0)
        snapshot('old defect fixed',0,0); candidate=snapshot('different new defect',0,1)
        self.assertEqual(nightly.audit(self.root,self.state,candidate,self.check),2)
        self.assertNotIn('RED culprit',(self.state/'last-result').read_text())
        self.assertEqual(nightly.git(self.root,'rev-parse','HEAD'),candidate)

    def test_log_retention_bounds_owned_outputs_and_preserves_current_and_unrelated(self):
        from unittest.mock import patch
        self.state.mkdir()
        current='20261001T000000Z-'+'a'*12
        newer='20261002T000000Z-'+'b'*12
        oldest='20260930T000000Z-'+'c'*12
        for name in [current,newer,oldest]:
            for suffix in ['.log','.driver.log','.log.receipt.json']:
                (self.state/(name+suffix)).write_text('output'*10)
        (self.state/'last-result').write_text('running')
        (self.state/'operator-notes').write_text('private')
        link=self.state/('20260929T000000Z-'+'d'*12+'.log')
        link.symlink_to(self.state/'operator-notes')
        folder=self.state/('20260928T000000Z-'+'e'*12+'.log')
        folder.mkdir()
        with patch.object(nightly,'MAX_LOG_GROUPS',2), patch.object(nightly,'MAX_OUTPUT_BYTES',400):
            nightly.retain_logs(self.state,current)
        self.assertTrue((self.state/(current+'.log')).exists())
        self.assertTrue((self.state/(newer+'.log')).exists())
        self.assertFalse((self.state/(oldest+'.log')).exists())
        self.assertEqual((self.state/'last-result').read_text(),'running')
        self.assertEqual((self.state/'operator-notes').read_text(),'private')
        self.assertTrue(link.is_symlink()); self.assertTrue(folder.is_dir())
        # A tighter total-size limit evicts even a recent noncurrent group.
        with patch.object(nightly,'MAX_OUTPUT_BYTES',200):
            nightly.retain_logs(self.state,current)
        self.assertFalse((self.state/(newer+'.log')).exists())
        self.assertTrue((self.state/(current+'.log')).exists())

    def test_oversized_log_retains_a_visibly_truncated_tail_within_the_bound(self):
        from unittest.mock import patch
        self.state.mkdir()
        current='20261001T000000Z-'+'a'*12
        log=self.state/(current+'.log')
        log.write_bytes(b'beginning'+b'x'*256+b'failure at end')
        with patch.object(nightly,'MAX_LOG_BYTES',128):
            nightly.retain_logs(self.state,current)
        self.assertEqual(log.stat().st_size,128)
        self.assertTrue(log.read_bytes().startswith(b'[nightly: earlier output truncated'))
        self.assertTrue(log.read_bytes().endswith(b'failure at end'))

    def test_diagnosis_budget_exhaustion_does_not_blame_source(self):
        from unittest.mock import patch
        candidate=self.commit(True, 'candidate')
        with patch.object(nightly, 'AUDIT_TIMEOUT_SECONDS', 1):
            with self.assertRaisesRegex(RuntimeError, 'twelve-hour bound'):
                nightly.audit(self.root,self.state,candidate,self.check)
        self.assertIn('INCONCLUSIVE', (self.state/'last-result').read_text())
        self.assertNotIn('RED culprit', (self.state/'last-result').read_text())

    def test_nightly_includes_slow_tests_with_a_three_hour_attempt_limit(self):
        from unittest.mock import patch
        build=self.root/'scripts/build'; build.mkdir(parents=True)
        (build/'gate.py').write_text('NIGHTLY_PROTOCOL = 1\n')
        self.state.mkdir()
        calls=[]
        def execute(command, output, deadline, **_options):
            calls.append((command, deadline))
            if 'full' in command:
                log=Path(command[command.index('--log')+1])
                log.with_suffix(log.suffix+'.receipt.json').write_text('{"mode":"full"}')
        with patch.object(nightly.gate, 'execute', side_effect=execute), patch.object(nightly.time,'monotonic',return_value=100), \
                patch.object(nightly.full_capacity, 'reserve') as capacity, \
                patch.object(nightly.heavy, 'slot') as slot:
            def enter(*_args, **_kwargs):
                self.assertTrue(capacity.return_value.__enter__.called)
                return slot.return_value
            slot.side_effect = enter
            self.assertTrue(nightly.full_gate(self.root,self.state,'a'*40))
            self.assertEqual(slot.call_args.args, ('gate',))
        self.assertIn('--nightly', calls[0][0])
        self.assertEqual(calls[0][0][calls[0][0].index('--timeout')+1], '10800')
        self.assertGreaterEqual(calls[0][1]-100, 10800)

    def test_scheduler_units_keep_maintenance_out_of_the_build_queue(self):
        actor='agt-demo'
        units=install_schedules.units(Path('/home/example/rust-nightly'),Path('/home/example'),actor)
        self.assertIn('nightly.py --repository /home/example/rust-nightly --agent-id '+actor,units['secode-nightly.service'])
        self.assertNotIn('/heavy ',units['secode-nightly.service'])
        self.assertIn('ExecStopPost=/usr/bin/python3 /home/example/.local/lib/secode-release/nightly_failure.py', units['secode-nightly.service'])
        self.assertIn('ExecStart=/usr/bin/python3 /home/example/.local/lib/secode-release/disk_hygiene.py',units['secode-disk-hygiene.service'])
        self.assertNotIn('/heavy ',units['secode-disk-hygiene.service'])
        self.assertIn('OnCalendar=*-*-* 02:00:00',units['secode-nightly.timer'])
        self.assertIn('OnUnitInactiveSec=1h',units['secode-disk-hygiene.timer'])
        control=Path('/home/example/.local/lib/secode-release-snapshots')/('a'*40)
        updated=install_schedules.units(Path('/home/example/rust-nightly'),Path('/home/example'),actor,control)
        self.assertIn(str(control/'nightly.py'),updated['secode-nightly.service'])
        self.assertIn(str(control/'nightly_failure.py'),updated['secode-nightly.service'])
        self.assertIn(str(control/'retention.py'),install_schedules.retention_units(Path('/home/example'),control=control)['secode-retention.service'])
        with self.assertRaises(ValueError):install_schedules.units(Path('/tmp/root;echo'),Path('/home/example'),actor)
