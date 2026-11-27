"""Real lock contention and nightly skip attribution without running Cargo."""
import fcntl
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch,Mock

import gate
import nightly
import test_nightly as nightly_tests


class SlotTests(unittest.TestCase):
    def test_nightly_wait_bound_is_accepted_by_the_real_cli_parser(self):
        self.assertEqual(gate.parse_args(['full','--nightly','--wait','14400']).wait,14400)
        for arguments in [['full','--nightly','--wait','14401'],['full','--wait','14400']]:
            with self.subTest(arguments=arguments),patch('sys.stderr',new_callable=io.StringIO),self.assertRaises(SystemExit):
                gate.parse_args(arguments)

    def test_waited_conflict_reports_actual_kernel_holder(self):
        with tempfile.TemporaryDirectory() as name:
            lock=Path(name)/'full'
            with lock.open('a') as owner, patch.object(gate,'FULL_LOCK',lock), patch('heavy.shared',return_value=False):
                fcntl.flock(owner,fcntl.LOCK_EX)
                with self.assertRaisesRegex(gate.GateSkipped,r'slot busy \(holder PID '+str(os.getpid())+r'\)'):
                    gate.full_slot(.05)
                with lock.open('a') as peer:
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(peer,fcntl.LOCK_EX|fcntl.LOCK_NB)

    def test_waiting_acquires_after_release_and_keeps_lease(self):
        with tempfile.TemporaryDirectory() as name:
            lock=Path(name)/'full'
            script='import fcntl,sys;f=open(sys.argv[1],"a");fcntl.flock(f,fcntl.LOCK_EX);print("locked",flush=True);sys.stdin.readline()'
            with subprocess.Popen([sys.executable,'-c',script,str(lock)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True) as owner:
                self.assertEqual(owner.stdout.readline(),'locked\n')
                def release():
                    owner.stdin.write('release\n');owner.stdin.flush()
                timer=threading.Timer(.1,release);timer.start()
                try:
                    with patch.object(gate,'FULL_LOCK',lock),patch('heavy.shared',return_value=False):
                        with gate.full_slot(2),lock.open('a') as peer:
                            with self.assertRaises(BlockingIOError):
                                fcntl.flock(peer,fcntl.LOCK_EX|fcntl.LOCK_NB)
                    owner.wait(timeout=3)
                finally:
                    timer.join(timeout=3)
                    if owner.poll() is None:owner.kill();owner.wait(timeout=3)

    def test_crossing_into_measurement_window_stops_before_target_admission(self):
        with tempfile.TemporaryDirectory() as name:
            # The log path is given outright so this case does not depend on an
            # ambient build target, which a bare test runner may not export.
            args=gate.parse_args(['full','--nightly','--base','a'*40,'--log',str(Path(name)/'gate.log')])
            slot=Mock()
            with patch.object(gate,'parse_args',return_value=args),patch.object(gate,'output',side_effect=[b'/tmp',b'a'*40]), \
                 patch.object(gate,'changed_paths',return_value=set()),patch.object(gate.publication_scan,'check',return_value=[]),patch.object(gate.gate_plan,'required_mode',return_value='full'), \
                 patch.object(gate,'full_slot',return_value=slot), \
                 patch.object(gate,'nightly_start_allowed',side_effect=[None,gate.GateSkipped('measurement hold 12:00–13:00Z')]) as guard, \
                 patch.object(gate.evict_targets,'prepare') as prepare,patch('sys.stderr',new_callable=io.StringIO) as output:
                self.assertEqual(gate.main(),74)
            self.assertEqual(guard.call_count,2)
            self.assertIn('SKIPPED full: measurement hold',output.getvalue())
            prepare.assert_not_called();slot.close.assert_called_once()

    def test_window_uses_utc_boundaries(self):
        from datetime import datetime,timezone
        for hour in [11,12,13]:
            with patch('datetime.datetime') as clock:
                clock.now.return_value=datetime(2026,11,2,hour,0,tzinfo=timezone.utc)
                if hour==12:
                    with self.assertRaises(gate.GateSkipped):gate.nightly_start_allowed()
                else:gate.nightly_start_allowed()
                clock.now.assert_called_once_with(timezone.utc)


class SkipTests(unittest.TestCase):
    setUp=nightly_tests.NightlyTests.setUp
    tearDown=nightly_tests.NightlyTests.tearDown
    commit=nightly_tests.NightlyTests.commit
    # Keep only these inherited-fixture tests in this class; the original suite
    # separately exercises regression bisection and installed-controller behavior.
    def test_skip_without_baseline_is_not_a_green_result_or_an_alert(self):
        candidate=self.commit(True,'candidate')
        publish=Mock()
        with patch.object(nightly,'report_blocker') as report:
            self.assertEqual(nightly.audit(self.root,self.state,candidate,
                Mock(side_effect=nightly.NightlySkipped('slot busy (holder PID 42)')),
                dry_run=False,publish=publish),0)
        self.assertIn('skipped: slot busy (holder PID 42)',(self.state/'last-result').read_text())
        self.assertNotIn('PASS',(self.state/'last-result').read_text())
        self.assertNotEqual(subprocess.run(['git','rev-parse','--verify',nightly.GOOD_REF],cwd=self.root,capture_output=True).returncode,0)
        publish.assert_not_called();report.assert_not_called()

    def test_skip_during_diagnosis_preserves_green_and_never_reverts(self):
        good=self.commit(True,'green');nightly.git(self.root,'update-ref',nightly.GOOD_REF,good)
        candidate=self.commit(False,'candidate')
        calls=0
        def check(*args):
            nonlocal calls
            calls+=1
            if calls==1:return nightly.Check(False,failures=('actual source failure',))
            raise nightly.NightlySkipped('measurement hold 12:00–13:00Z')
        publish=Mock()
        self.assertEqual(nightly.audit(self.root,self.state,candidate,check,dry_run=False,publish=publish),0)
        self.assertEqual(nightly.git(self.root,'rev-parse',nightly.GOOD_REF),good)
        self.assertEqual(nightly.git(self.root,'rev-parse','HEAD'),candidate)
        publish.assert_not_called()

    def test_capacity_wait_and_child_deadline_fit_remaining_audit_budget(self):
        build=self.root/'scripts/build';build.mkdir(parents=True)
        (build/'gate.py').write_text('NIGHTLY_PROTOCOL = 1\nNIGHTLY_WAIT_PROTOCOL = 1\nFULL_CAPACITY_PROTOCOL = 1\n')
        self.state.mkdir()
        calls=[]
        def execute(command,output,deadline,**kwargs):
            calls.append((command,deadline))
            if command[:3]==['bash','scripts/gate.sh','full']:
                log=Path(command[command.index('--log')+1])
                log.with_suffix(log.suffix+'.receipt.json').write_text('{"mode":"full"}')
        with patch.object(nightly.full_capacity,'reserve') as capacity, \
             patch.object(nightly.heavy,'slot') as slot,patch.object(nightly.gate,'execute',side_effect=execute), \
             patch.object(nightly.gate,'nightly_start_allowed'),patch.object(nightly.time,'monotonic',return_value=100):
            self.assertTrue(nightly.full_gate(self.root,self.state,'a'*40,deadline=12000))
        self.assertEqual(slot.call_args.kwargs['wait'],950)
        self.assertEqual(capacity.call_args.args,(950,))
        self.assertNotIn('--wait',calls[0][0])
        self.assertLessEqual(calls[0][1],12000)
        self.assertGreaterEqual(calls[0][1]-100,nightly.FULL_TIMEOUT_SECONDS)

    def test_only_typed_skip_exit_and_marker_skip_a_full_attempt(self):
        build=self.root/'scripts/build';build.mkdir(parents=True)
        (build/'gate.py').write_text('NIGHTLY_PROTOCOL = 1\nNIGHTLY_WAIT_PROTOCOL = 1\nFULL_CAPACITY_PROTOCOL = 1\n')
        self.state.mkdir()
        for code,marker,skips in [(74,'SKIPPED full: slot busy (holder PID 42)',True),
                                  (74,'unrelated exit',False),(75,'SKIPPED full: slot busy (holder PID 42)',False),
                                  (1,'SKIPPED full: slot busy (holder PID 42)',False)]:
            with self.subTest(code=code,marker=marker):
                def execute(command,output,deadline,**kwargs):
                    self.assertNotIn('--wait',command)
                    self.assertGreaterEqual(deadline-time.monotonic(),10850)
                    output.write(marker+'\n');output.flush()
                    raise subprocess.CalledProcessError(code,command)
                with patch.object(nightly.full_capacity,'reserve') as capacity, \
                     patch.object(nightly.heavy,'slot'),patch.object(nightly.gate,'execute',side_effect=execute), \
                     patch.object(nightly.gate,'nightly_start_allowed'):
                    if skips:
                        with self.assertRaisesRegex(nightly.NightlySkipped,'slot busy'):nightly.full_gate(self.root,self.state,'a'*40)
                    else:
                        self.assertFalse(nightly.full_gate(self.root,self.state,'a'*40))
                    self.assertEqual(capacity.call_args.args,(14400,))


if __name__=='__main__':unittest.main()
