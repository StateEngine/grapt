"""Real Git and ordinary affected execution for explicitly reviewed TUI inputs."""
from contextlib import contextmanager
import json
from pathlib import Path
import subprocess
import sys
import unittest

import gate
import reviewed_composition as reviewed
from test_landing import command, commit, invoke
from test_landing_documents import fixture as compiler_fixture, request_input

MAIN = 'crates/grapt-tui/src/main.rs'


@contextmanager
def fixture():
    with compiler_fixture() as (outer, canonical, root, base, env):
        # The retained full result is synthetic fixture input. The fresh
        # reviewed result below is produced by actual gate commands on Rust.
        source = root / MAIN
        source.parent.mkdir(parents=True)
        source.write_text('fn main() {}\n#[test]\nfn fixture_target() {}\n')
        (source.parent.parent / 'Cargo.toml').write_text(
            '[package]\nname="grapt-tui"\nversion="0.1.0"\nedition="2021"\n')
        manifest = root / 'Cargo.toml'
        manifest.write_text(manifest.read_text().replace('members=[', 'members=["crates/grapt-tui",'))
        command(root, 'cargo', 'generate-lockfile', '--offline', env=env)
        full = commit(root, [MAIN, 'crates/grapt-tui/Cargo.toml', 'Cargo.toml', 'Cargo.lock'])
        original = request_input(outer, root, base, full, full=True)
        retained = dict(commit=full, base=base, log=original['gate_log'])
        # An accepted canonical advance changes a source path whose authored
        # patch was empty. Its bytes must not be mistaken for new owner work.
        (canonical / 'a/src/lib.rs').write_text('// canonical input policy\npub fn value() -> u32 {\n    1\n}\n')
        advanced = commit(canonical, ['a/src/lib.rs'])
        command(root, 'git', 'fetch', '-q', str(canonical), 'main')
        command(root, 'git', 'rebase', advanced)
        source.write_text('fn main() {\n    println!("reviewed input adapter");\n}\n#[test]\nfn fixture_target() {}\n')
        candidate = commit(root, [MAIN])
        log = outer / 'reviewed.log'
        request = dict(original, accepted=candidate, old_base=advanced, full=False,
                       gate_log=str(log), retained_full=retained)
        yield outer, canonical, root, advanced, env, request


def gate_command(request):
    return ['bash', 'scripts/gate.sh', 'reviewed', '--base', request['old_base'],
            '--log', request['gate_log'],
            *[arg for k in ('commit', 'base', 'log') for arg in
              ('--retained-' + k, request['retained_full'][k])]]


def run(root, env, argv):
    return subprocess.run(argv, cwd=root, env=env, capture_output=True, text=True, timeout=90)


class ReviewedCompositionTests(unittest.TestCase):
    def test_real_affected_gate_retains_full_and_qualifies_exact_reviewed_source(self):
        with fixture() as (_, _, root, base, env, request):
            old_path = Path(request['retained_full']['log']).with_suffix('.log.receipt.json')
            old = old_path.read_bytes()
            result = run(root, env, gate_command(request))
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            log = Path(request['gate_log'])
            record = json.loads(log.with_suffix('.log.receipt.json').read_text())
            self.assertEqual(record['mode'], 'reviewed')
            self.assertEqual(record['base'], base)
            self.assertEqual(record['reviewed_composition']['fresh_paths'], [MAIN])
            self.assertTrue(any(command[:3] == ['cargo', 'nextest', 'run']
                                and 'grapt-tui' in command for command in record['commands']))
            self.assertIn('cargo check --locked --workspace --tests', log.read_text())
            self.assertIn('write_chokepoint', log.read_text())
            self.assertEqual(old_path.read_bytes(), old)
            result = invoke(root, request, env, 'qualify')
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            # Ordinary/full verifiers cannot silently borrow this result.
            args = [sys.executable, 'scripts/build/receipt.py', str(log), request['accepted'], '--base', base]
            self.assertNotEqual(run(root, env, args).returncode, 0)
            self.assertNotEqual(run(root, env, args + ['--require-full']).returncode, 0)
            self.assertNotEqual(invoke(root, request, env, 'compose').returncode, 0)
            self.assertNotEqual(invoke(root, request, env, 'verify').returncode, 0)
            self.assertNotEqual(run(root, env, gate_command(request)).returncode, 0)
            self.assertEqual(old_path.read_bytes(), old)
            # Missing fresh command, altered retained result and wrong base refuse.
            receipt_path = log.with_suffix('.log.receipt.json')
            saved = receipt_path.read_bytes()
            record['commands'] = []
            receipt_path.write_text(json.dumps(record))
            self.assertNotEqual(invoke(root, request, env, 'qualify').returncode, 0)
            receipt_path.write_bytes(saved)
            old_path.write_bytes(old + b'\n')
            self.assertNotEqual(invoke(root, request, env, 'qualify').returncode, 0)
            old_path.write_bytes(old)
            wrong = dict(request, old_base=request['retained_full']['base'])
            self.assertNotEqual(invoke(root, wrong, env, 'qualify').returncode, 0)
            wrong = dict(request, accepted=request['retained_full']['commit'])
            self.assertNotEqual(invoke(root, wrong, env, 'qualify').returncode, 0)
            (root / 'README.md').write_text('uncommitted owner edit\n')
            self.assertNotEqual(invoke(root, request, env, 'qualify').returncode, 0)

    def test_protected_patch_changes_refuse_before_checks(self):
        for path in ('Cargo.toml', 'Cargo.lock', 'a/src/lib.rs',
                     'scripts/build/gate_policy.py', 'crates/grapt-product/src/permissions.rs',
                     'docs/extra.md', MAIN.lower().replace('main', 'other')):
            with self.subTest(path=path), fixture() as (_, _, root, _, env, request):
                p = root / path
                p.parent.mkdir(parents=True, exist_ok=True)
                with p.open('a') as output:
                    output.write('\n# unqualified change\n' if path.endswith(('.toml', '.lock', '.py')) else '\n// unqualified change\n')
                request['accepted'] = commit(root, [path])
                result = run(root, env, gate_command(request))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('outside the retained full input scope', result.stdout + result.stderr)
                self.assertFalse(Path(request['gate_log']).with_suffix('.log.receipt.json').exists())

    def test_missing_wrong_stale_affected_and_changed_full_refuse(self):
        for mutation in ('missing', 'wrong-source', 'wrong-base', 'stale', 'affected', 'log', 'commands'):
            with self.subTest(mutation=mutation), fixture() as (_, _, root, _, env, request):
                retained = request['retained_full']
                path = Path(retained['log']).with_suffix('.log.receipt.json')
                record = json.loads(path.read_text())
                if mutation == 'missing':
                    path.unlink()
                elif mutation == 'wrong-source':
                    retained['commit'] = request['accepted']
                elif mutation == 'wrong-base':
                    retained['base'] = request['old_base']
                elif mutation == 'log':
                    Path(retained['log']).write_text('changed result\n')
                else:
                    if mutation == 'stale': record['finished_unix'] = 0
                    if mutation == 'affected': record['mode'] = 'affected'
                    if mutation == 'commands': record['commands'] = []
                    path.write_text(json.dumps(record))
                self.assertNotEqual(run(root, env, gate_command(request)).returncode, 0)
                self.assertFalse(Path(request['gate_log']).with_suffix('.log.receipt.json').exists())

    def test_dirty_wrong_head_and_type_changes_refuse(self):
        for mutation in ('dirty', 'wrong-head', 'symlink', 'executable', 'binary', 'large', 'delete'):
            with self.subTest(mutation=mutation), fixture() as (_, _, root, _, env, request):
                p = root / MAIN
                if mutation == 'dirty':
                    p.write_text('uncommitted\n')
                elif mutation == 'wrong-head':
                    command(root, 'git', 'checkout', '-q', '--detach', request['retained_full']['commit'])
                else:
                    if mutation == 'symlink':
                        p.unlink(); p.symlink_to('../../../README.md')
                    if mutation == 'executable': p.chmod(0o755)
                    if mutation == 'binary': p.write_bytes(b'\0')
                    if mutation == 'large': p.write_bytes(b'x' * (1024 * 1024 + 1))
                    if mutation == 'delete': p.unlink()
                    request['accepted'] = commit(root, [MAIN])
                self.assertNotEqual(run(root, env, gate_command(request)).returncode, 0)
                self.assertFalse(Path(request['gate_log']).with_suffix('.log.receipt.json').exists())

    def test_accepted_base_test_edits_preserve_only_the_qualified_authored_patch(self):
        path = 'crates/grapt-product/tests/dogfood_headless/file_undo.rs'
        with fixture() as (_, canonical, root, _, env, request):
            # Build a real Git history with separated hunks on the same test.
            # The owner stack is replayed unchanged onto the accepted base.
            retained = request['retained_full']
            command(root, 'git', 'checkout', '-q', '--detach', retained['base'])
            source = root / path
            source.parent.mkdir(parents=True)
            text = '// base start\n' + '// unchanged\n' * 30 + '// base end\n'
            source.write_text(text)
            old_base = commit(root, [path])
            source.write_text(text.replace('base end', 'qualified owner end'))
            old_full = commit(root, [path])
            command(root, 'git', 'checkout', '-q', '--detach', old_base)
            source.write_text(text.replace('base start', 'accepted canonical start'))
            new_base = commit(root, [path])
            command(root, 'git', 'cherry-pick', old_full)
            candidate = command(root, 'git', 'rev-parse', 'HEAD')
            retained = dict(retained, commit=old_full, base=old_base)
            self.assertNotEqual(reviewed.patch(root, old_base, old_full)[path],
                                reviewed.patch(root, new_base, candidate)[path])
            self.assertEqual(reviewed.coverage(root, candidate, new_base, retained), [path])
            # A new owner edit cannot borrow the accepted canonical change.
            with source.open('a') as output:
                output.write('// unqualified owner work\n')
            changed = commit(root, [path])
            with self.assertRaisesRegex(ValueError, 'outside the retained full input scope'):
                reviewed.coverage(root, changed, new_base, retained)
            # Overlapping canonical hunks still refuse; no merge drivers run.
            command(root, 'git', 'checkout', '-q', '--detach', old_base)
            source.write_text(text.replace('base end', 'conflicting canonical end'))
            conflicting = commit(root, [path])
            source.write_text(text.replace('base end', 'qualified owner end'))
            conflict_candidate = commit(root, [path])
            with self.assertRaisesRegex(ValueError, 'conflict'):
                reviewed.coverage(root, conflict_candidate, conflicting, retained)

    def test_runtime_failure_in_fresh_input_prevents_receipt(self):
        with fixture() as (_, _, root, _, env, request):
            (root / MAIN).write_text('fn main() {}\n#[test]\nfn input_regression() {\n    panic!("fresh input fails");\n}\n')
            request['accepted'] = commit(root, [MAIN])
            result = run(root, env, gate_command(request))
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('input_regression ... FAILED', Path(request['gate_log']).read_text())
            self.assertFalse(Path(request['gate_log']).with_suffix('.log.receipt.json').exists())

    def test_explicit_arguments_only_and_no_original_gate_overwrite(self):
        retained = ['--retained-commit', 'a'*40, '--retained-base', 'b'*40, '--retained-log', '/tmp/full.log']
        for argv in (['reviewed'], ['affected', *retained], ['full', *retained],
                     ['reviewed', *retained, '--log', '/tmp/new.log', '--row-a'],
                     ['reviewed', *retained, '--log', '/tmp/new.log', '--nightly']):
            with self.subTest(argv=argv), self.assertRaises(SystemExit):
                gate.parse_args(argv)
        with fixture() as (_, _, root, _, env, request):
            old = Path(request['retained_full']['log']).read_bytes()
            request['gate_log'] = request['retained_full']['log']
            self.assertNotEqual(run(root, env, gate_command(request)).returncode, 0)
            self.assertEqual(Path(request['gate_log']).read_bytes(), old)


if __name__ == '__main__':
    unittest.main()
