"""A failing gate leg stops its sibling before qualification can finish."""
import os
from pathlib import Path
import subprocess
import signal
import threading
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import gate
import gate_parallel
import heavy


class ParallelTests(unittest.TestCase):
    def test_native_sidecar_is_skipped_without_opt_in(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(gate_parallel.windows_gate, 'native_command') as prepare, \
                patch.object(heavy, 'owned_process') as launch:
            for argv in (['full'], ['full', '--nightly'], ['affected']):
                args = gate.parse_args(argv)
                with self.subTest(argv=argv), gate_parallel.native(
                        Path(directory) / 'gate.log', commit='a' * 40,
                        packages=['grapt-types'], workspace=args.nightly, nightly=args.nightly,
                        deadline=time.monotonic() + 300, requested=args.native_windows) as native:
                    self.assertIsNone(native)
            prepare.assert_not_called()
            launch.assert_not_called()

    def test_landing_selects_operational_native_runner_and_candidate_policy(self):
        from contextlib import nullcontext
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory);process=Mock();process.poll.return_value=0
            policy=root/'scripts/build/windows_gate.py'
            with patch.object(gate_parallel.windows_gate,'__file__',str(policy)), \
                    patch.object(heavy,'owned_process',return_value=nullcontext(process)) as launch, \
                    patch.object(gate_parallel.NativeLeg,'finish'):
                with gate_parallel.native(root/'gate.log',commit='a'*40,
                        packages=['grapt-types'],deadline=time.monotonic()+30,requested=True):
                    pass
            command=launch.call_args.args[0]
            self.assertEqual(Path(command[1]),Path(gate_parallel.__file__).with_name('windows_native.py'))
            self.assertEqual(command[command.index('--source-policy')+1],str(root))

    def test_native_capacity_exit_is_not_a_test_failure(self):
        with subprocess.Popen([sys.executable, '-c', 'raise SystemExit(75)']) as process:
            process.wait(timeout=5)
            leg = gate_parallel.NativeLeg(process, Path('/absent/native.json'), 'a'*40,
                                          ['grapt-types'], False, False, time.monotonic()+5)
            with self.assertRaises(gate_parallel.CapacityUnavailable):
                leg.check()

    def test_concurrent_legs_join_after_either_side_fails(self):
        program = r'''
import os,signal,sys,time
from pathlib import Path
root=Path(sys.argv[1]); name=sys.argv[2]; fail=sys.argv[3]
(root/(name+'-pid')).write_text(str(os.getpid()))
def cancel(*_): raise KeyboardInterrupt
if name=='native':
    sys.path.insert(0,sys.argv[4])
    from windows_native import interrupt
    signal.signal(signal.SIGTERM,interrupt)
else:
    signal.signal(signal.SIGTERM,cancel)
(root/(name+'-ready')).touch()
try:
    deadline=time.monotonic()+10
    other='linux' if name=='native' else 'native'
    while not (root/(other+'-ready')).exists():
        if time.monotonic()>deadline:raise TimeoutError('sibling never started')
        time.sleep(.01)
    if name==fail:raise SystemExit(7)
    while True:time.sleep(.01)
except KeyboardInterrupt:
    (root/(name+'-joining')).touch()
    time.sleep(.2)
    (root/(name+'-joined')).touch()
'''
        for failing in ('native', 'linux'):
            with self.subTest(failing=failing), tempfile.TemporaryDirectory() as directory:
                root=Path(directory); deadline=time.monotonic()+15
                def command(name):return [sys.executable,'-B','-c',program,str(root),name,failing,str(Path(gate_parallel.__file__).parent)]
                stopped=threading.Event(); repeated=threading.Event()
                def repeat_cancellation():
                    while not stopped.wait(.005):
                        if failing=='linux' and (root/'native-joining').exists():
                            fd=os.pidfd_open(int((root/'native-pid').read_text()))
                            try:signal.pidfd_send_signal(fd,signal.SIGTERM)
                            finally:os.close(fd)
                            repeated.set();return
                thread=threading.Thread(target=repeat_cancellation);thread.start()
                self.addCleanup(thread.join,10)
                self.addCleanup(stopped.set)
                with patch.object(gate_parallel.windows_gate,'native_command',return_value=command('native')), \
                        patch.object(gate_parallel.windows_gate,'verify_native') as verify:
                    with (root/'linux.log').open('w') as log:
                        with self.assertRaises(subprocess.CalledProcessError):
                            with gate_parallel.native(root/'gate.log',commit='a'*40,
                                    packages=['fixture'],deadline=deadline,requested=True) as native:
                                gate.execute(command('linux'),log,deadline,cwd=root,check=native.check)
                    verify.assert_not_called()
                stopped.set();thread.join(10)
                if failing=='linux':self.assertTrue(repeated.is_set())
                for name in ('native','linux'):
                    self.assertTrue((root/(name+'-ready')).exists())
                    self.assertFalse(heavy.live_group(int((root/(name+'-pid')).read_text())))
                self.assertTrue((root/(('linux' if failing=='native' else 'native')+'-joined')).exists())
