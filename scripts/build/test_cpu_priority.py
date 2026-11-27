"""Weighted gate placement must keep memory custody and joined shutdown."""
import os
import subprocess
import sys
import uuid
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import cpu_priority
import heavy


class PriorityTests(unittest.TestCase):
    def test_only_an_active_uncapped_weighted_slice_replaces_load_admission(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            gate = root/cpu_priority.SLICE
            background = root/'app.slice'
            gate.mkdir(); background.mkdir()
            (gate/'cpu.weight').write_text('1000\n')
            (background/'cpu.weight').write_text('100\n')
            with patch.object(cpu_priority, 'CGROUP', root):
                self.assertFalse(cpu_priority.configured())
                (gate/'cpu.max').write_text('max 100000\n')
                self.assertTrue(cpu_priority.configured())
                (gate/'cpu.max').write_text('100000 100000\n')
                self.assertFalse(cpu_priority.configured())

    def test_scope_failure_joins_writers_and_never_moves_an_inherited_ram_governor(self):
        with patch.object(cpu_priority, 'active', return_value=False), \
                patch.object(cpu_priority, 'configured', return_value=True), \
                patch.object(heavy, 'inherited_fds', return_value=()), \
                patch.object(heavy, 'run', side_effect=RuntimeError('owned command failed')) as run, \
                patch.object(cpu_priority, 'state', return_value={'LoadState': 'not-found'}), \
                patch.object(cpu_priority, 'stop') as stop:
            with patch.dict(os.environ, {'SECODE_RAM_TARGET_RUN': '/owned/ram'}):
                with self.assertRaisesRegex(ValueError, 'Preserve the inherited RAM governor'):
                    cpu_priority.run(['owned-command'])
            run.assert_not_called(); stop.assert_not_called()
            environment = dict(os.environ)
            environment.pop('SECODE_RAM_TARGET_RUN', None)
            with patch.dict(os.environ, environment, clear=True):
                with self.assertRaisesRegex(RuntimeError, 'owned command failed'):
                    cpu_priority.run(['owned-command'])
            stop.assert_called_once()

        # A crashed gate cannot leave a RAM worker in a separately governed
        # scope, even when that worker escapes the original process group.
        parent_slice = 'secodeprioritytest' + uuid.uuid4().hex + '.slice'
        worker = r"""
import json,os,subprocess,sys,time
from pathlib import Path
sys.path.insert(0,sys.argv[4])
import cpu_priority
cpu_priority.SLICE=sys.argv[2]
root=Path(sys.argv[1])
unit='secode-ram-run-'+sys.argv[3]+'.scope'
(root/'unit').write_text(unit)
writer='import os,signal,time; from pathlib import Path\nchild=os.fork()\nif child == 0:\n os.setsid(); signal.signal(signal.SIGTERM,signal.SIG_IGN); Path('+repr(str(root/'pid'))+').write_text(str(os.getpid())); time.sleep(120)\nelse: os.waitpid(child,0)'
process=subprocess.Popen(['systemd-run','--user','--scope','--quiet','--collect',
 '--slice='+cpu_priority.run_slice(),'--unit='+unit,'-p','MemoryMax=64M',
 '-p','MemorySwapMax=0','-p','TimeoutStopSec=2','--',sys.executable,'-c',writer],
 stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=(root/'ram-error').open('w'),start_new_session=True)
deadline=time.monotonic()+10
while not (root/'pid').exists():
 if process.poll() is not None: raise RuntimeError('RAM scope did not start: '+(root/'ram-error').read_text())
 if time.monotonic()>=deadline: raise TimeoutError('RAM writer did not start')
 time.sleep(.01)
os._exit(9)
"""
        with tempfile.TemporaryDirectory() as temporary, \
                patch.object(cpu_priority, 'SLICE', parent_slice), \
                patch.object(cpu_priority, 'configured', return_value=True):
            root = Path(temporary)
            try:
                code = cpu_priority.run([sys.executable, '-B', '-c', worker,
                    str(root), parent_slice, uuid.uuid4().hex, str(Path(cpu_priority.__file__).parent)], timeout=20)
                self.assertEqual(code, 9)
                pid = int((root/'pid').read_text())
                try:
                    status = Path('/proc') / str(pid) / 'stat'
                    self.assertIn(status.read_text().rsplit(')', 1)[1].split()[0], ('Z', 'X'))
                except FileNotFoundError:
                    pass
                state = cpu_priority.state((root/'unit').read_text())
                self.assertIn(state['ActiveState'], ('inactive', 'failed'))
            finally:
                # This UUID parent belongs only to this regression, including
                # failure before the priority runner can perform its cleanup.
                subprocess.run(['systemctl', '--user', 'stop', parent_slice],
                    check=True, capture_output=True, timeout=15)
