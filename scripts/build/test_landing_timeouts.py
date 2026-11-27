"""Composition budgets and timeout propagation at the actual runner boundary."""
from contextlib import ExitStack, nullcontext, redirect_stderr, redirect_stdout
import io
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import gate
import landing


class LandingTimeoutTests(unittest.TestCase):
    def run_checks(self, root, mode, error=None):
        log = root/'composition.log'
        with patch.object(landing.importlib, 'reload', side_effect=lambda module: module), \
                patch.object(gate, 'execute', side_effect=error) as execute, \
                patch.object(landing.time, 'monotonic', return_value=50_000):
            landing.run_checks(root, [['selected check']], log, 'base', mode=mode)
        return execute.call_args

    def test_selected_mode_bounds_after_queue_wait(self):
        for mode, seconds in [('affected', 5400), ('full', gate.FULL_GATE_TIMEOUT_SECONDS),
                              ('composition', gate.REVIEWED_GATE_TIMEOUT_SECONDS)]:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                call = self.run_checks(Path(directory), mode)
                command, _, deadline = call.args
                self.assertEqual(command[command.index('--timeout') + 1], str(seconds))
                # Time already spent waiting for the landing lease is outside
                # this execution budget; cleanup has its own 100-second margin.
                self.assertEqual(deadline, 50_000 + seconds + 100 + (10800 if mode == 'full' else 0))
                self.assertIn(mode, command)
                self.assertNotIn('--exclusive-full', command)
                if mode == 'full':
                    self.assertEqual(command[command.index('--wait') + 1], '10800')
                    # Both generations use ordinary writer-aware admission;
                    # old candidates retain their policy via the transported runner.
                    with patch.object(gate, 'full_slot', lambda wait=0: None):
                        legacy, budget = landing.gate_invocation(Path(directory), gate, mode,
                                                                 'base', seconds, 'fresh.log')
                    self.assertEqual(legacy[:4], [sys.executable, '-B', str(Path(landing.__file__).with_name('gate.py')), 'full'])
                    self.assertEqual(legacy[legacy.index('--source-policy') + 1], directory)
                    self.assertNotIn('--exclusive-full', legacy)
                    self.assertEqual(legacy[legacy.index('--wait') + 1], '10800')
                    self.assertEqual(budget, 10800 + seconds + 100)
                    policy = Path(directory) / 'scripts/build'
                    policy.mkdir(parents=True)
                    (Path(directory) / '.git').mkdir()
                    for name in ('gate_scope', 'gate_policy', 'slow_tests', 'windows_gate', 'gate_plan'):
                        shutil.copyfile(Path(gate.__file__).with_name(name + '.py'), policy / (name + '.py'))
                    (policy / 'test_features.py').write_text('def arguments(*args): return ["checkout-policy"]\n')
                    probe = ("import runpy,sys; from pathlib import Path; sys.argv=sys.argv[1:]; "
                             "sys.path.insert(0,str(Path(sys.argv[0]).parent)); "
                             "exec(\"try: runpy.run_path(sys.argv[0],run_name='__main__')\\nexcept SystemExit as e: assert e.code == 0\"); "
                             "assert sys.modules['test_features'].arguments() == ['checkout-policy']; "
                             "import subprocess; subprocess.run([sys.executable,'-c','import ram_targets'], "
                             "cwd=Path(sys.argv[0]).parent,check=True,timeout=10)")
                    # Use the emitted interpreter flags, rather than adding
                    # -B in the probe and hiding a private-helper cleanup leak.
                    helper = Path(directory) / 'transport'
                    helper.mkdir()
                    for path in Path(landing.__file__).parent.glob('*.py'):
                        if not path.name.startswith('test_') or path.name == 'test_features.py':
                            shutil.copyfile(path, helper / path.name)
                    legacy[legacy.index(str(Path(landing.__file__).with_name('gate.py')))] = str(helper / 'gate.py')
                    result = subprocess.run([*legacy[:2], '-c', probe, legacy[2],
                        '--help', '--source-policy', directory], cwd=directory,
                        capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    native_result = subprocess.run([sys.executable,'-B','-c',probe,
                        str(helper/'windows_native.py'),'--help','--source-policy',directory],
                        cwd=directory,capture_output=True,text=True,timeout=10)
                    self.assertEqual(native_result.returncode,0,native_result.stderr)
                    for path in helper.iterdir():
                        path.unlink()
                    helper.rmdir()

    def test_initial_qualification_forwards_its_selected_execution_bound(self):
        for full, seconds in [(False, gate.REVIEWED_GATE_TIMEOUT_SECONDS),
                              (True, gate.parse_args(['full']).timeout)]:
            with self.subTest(full=full), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                request = dict(accepted='candidate', old_base='base', branch='owner',
                               run_gate=True, full=full, receipt=str(root/'receipt.json'),
                               gate_log=str(root/'gate.log'))
                with (root/'lease').open('w') as lease, ExitStack() as stack:
                    stack.enter_context(patch.object(landing, 'apply_policy', return_value=request))
                    stack.enter_context(patch.object(landing, 'source'))
                    stack.enter_context(patch.object(landing.heavy, 'ram_compatible', return_value=False))
                    stack.enter_context(patch.object(landing.heavy, 'nvme_target', return_value=None))
                    stack.enter_context(patch.object(landing.heavy, 'slot', return_value=nullcontext(lease)))
                    stack.enter_context(patch.object(landing.full_capacity, 'reserve', return_value=nullcontext()))
                    stack.enter_context(patch.object(landing, 'compose_locked', return_value='composed'))
                    stack.enter_context(patch.object(landing.time, 'monotonic', return_value=50_000))
                    execute = stack.enter_context(patch.object(gate, 'execute'))
                    self.assertEqual(landing.compose(root, request), 'composed')
                command, _, deadline = execute.call_args.args
                self.assertIn('--timeout', command)
                self.assertEqual(command[command.index('--timeout') + 1], str(seconds))
                self.assertIn('full' if full else 'affected', command)
                self.assertNotIn('--exclusive-full', command)
                self.assertEqual(deadline, 50_000 + seconds + 100 + (10800 if full else 0))

    def test_child_timeout_names_execution_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(RuntimeError, 'composition timed out at 5400 s'):
                self.run_checks(Path(directory), 'affected',
                                subprocess.CalledProcessError(124, ['gate']))

    def test_test_failure_stays_a_test_failure(self):
        failure = subprocess.CalledProcessError(7, ['cargo', 'test'])
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaises(subprocess.CalledProcessError) as caught:
                self.run_checks(Path(directory), 'affected', failure)
        self.assertIs(caught.exception, failure)

    def test_supervisor_timeout_names_its_cleanup_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(RuntimeError, 'supervisor timed out at 5500 s'):
                self.run_checks(Path(directory), 'affected', gate.GateTimeout('deadline'))

    def test_gate_exit_status_distinguishes_timeout_from_command_failure(self):
        for timed_out in (False, True):
            with self.subTest(timed_out=timed_out), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                target = root/'target'
                target.mkdir()
                args = gate.parse_args(['affected', '--timeout', '5400', '--log', str(root/'gate.log')])
                error = (gate.GateTimeout('deadline') if timed_out else
                         subprocess.CalledProcessError(7, ['fixture']))
                def output(*command):
                    if command == ('git', 'rev-parse', '--show-toplevel'):
                        return str(root).encode()
                    if command[:2] == ('cargo', 'metadata'):
                        return b'{"packages":[],"workspace_members":[]}'
                    return b'base'
                stderr = io.StringIO()
                with ExitStack() as stack:
                    lease = stack.enter_context((root/'lease').open('w'))
                    for name, value in [('parse_args', args), ('output', output),
                                        ('changed_paths', set()), ('claim_target', None)]:
                        stack.enter_context(patch.object(gate, name,
                            **({'side_effect': value} if name == 'output' else {'return_value': value})))
                    stack.enter_context(patch.object(gate.evict_targets, 'prepare', return_value=lease))
                    stack.enter_context(patch.object(gate.publication_scan, 'check', return_value=[]))
                    stack.enter_context(patch.object(gate.evict_targets, 'inherit'))
                    stack.enter_context(patch.object(gate.gate_evidence, 'preserve'))
                    stack.enter_context(patch.object(gate.receipt, 'source', return_value={}))
                    receipt = stack.enter_context(patch.object(gate.receipt, 'write'))
                    stack.enter_context(patch.object(gate.gate_plan, 'commands', return_value=[['fixture']]))
                    stack.enter_context(patch.object(gate, 'execute', side_effect=error))
                    stack.enter_context(patch.object(gate.signal, 'signal'))
                    stack.enter_context(patch.dict(os.environ, {
                        'SECODE_TARGET_ROOT': str(root), 'CARGO_TARGET_DIR': str(target)}))
                    stack.enter_context(redirect_stdout(io.StringIO()))
                    stack.enter_context(redirect_stderr(stderr))
                    self.assertEqual(gate.main(), 124 if timed_out else 1)
                    receipt.assert_not_called()
                if timed_out:
                    self.assertIn('Gate timed out at 5400 s', stderr.getvalue())
                else:
                    self.assertIn('exit status 7', stderr.getvalue())
                    self.assertNotIn('timed out', stderr.getvalue())
