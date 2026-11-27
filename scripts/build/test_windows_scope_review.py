"""Judge batch omission through command planning and receipt verification."""
from contextlib import chdir
import json
import subprocess
import unittest
from unittest.mock import patch

from pathlib import Path

import gate_plan
import receipt
import windows_scope
from fixture_environment import isolated_environment, nextest_result
from test_receipt import landing_repository, commit_paths


class WindowsScopeReview(unittest.TestCase):
    def changed(self, root, base):
        return set(subprocess.check_output(
            ['git', 'diff', '--no-renames', '--name-only', base, 'HEAD'],
            cwd=root, text=True).splitlines())

    def test_a_rust_change_keeps_cross_checks_and_linux_commands(self):
        with landing_repository() as (root, base):
            (root/'a/src/lib.rs').write_text('pub fn changed() {}\n')
            commit_paths(root, ['a/src/lib.rs'])
            paths = self.changed(root, base)
            packages = json.loads(subprocess.check_output(
                ['cargo', 'metadata', '--locked', '--no-deps', '--format-version=1'],
                cwd=root, env=isolated_environment()))['packages']
            for mode in ('affected', 'full'):
                ordinary = gate_plan.commands(mode, packages, paths, root, base=base)
                batch = gate_plan.commands(mode, packages, paths, root, base=base, batch=True)
                self.assertTrue(any(c[:2] == ['cargo', 'xwin'] for c in ordinary))
                self.assertEqual(batch, ordinary)
                nightly = gate_plan.commands(mode, packages, paths, root, base=base,
                                             batch=True, nightly=True)
                self.assertTrue(any(c[:2] == ['cargo', 'xwin'] for c in nightly))

    def test_a_unix_only_import_still_runs_the_windows_checks(self):
        # The old rule read the blob for windows markers and would have skipped
        # this unconditional unix import, which breaks a Windows build.
        with landing_repository() as (root, base):
            source = root/'a/src/lib.rs'
            source.write_text('use std::os::unix::fs::PermissionsExt;\n'
                              'use std::path::Path;\n'
                              'pub fn mode() -> Vec<&\'static str> {\n'
                              '    let _ = Path::new("/tmp/x");\n'
                              '    "/tmp/x".split(\'/\').collect()\n'
                              '}\n')
            commit_paths(root, ['a/src/lib.rs'])
            paths = self.changed(root, base)
            packages = json.loads(subprocess.check_output(
                ['cargo', 'metadata', '--locked', '--no-deps', '--format-version=1'],
                cwd=root, env=isolated_environment()))['packages']
            commands = gate_plan.commands('affected', packages, paths, root, base=base, batch=True)
            self.assertTrue(any(c[:2] == ['cargo', 'xwin'] for c in commands))

    def test_documentation_and_box_scripts_alone_may_skip_the_checks(self):
        inert = ('docs/guide.md', 'README.md', 'AGENTS.md', 'notices/10-adoptions/x.md',
                 'scripts/build/disk_hygiene.py', 'scripts/ci/run.py', 'scripts/daily-use/x.py')
        for path in inert:
            with self.subTest(path=path):
                self.assertFalse(windows_scope.required({path}))
        self.assertFalse(windows_scope.required(set(inert)))

    def test_every_build_input_and_unknown_path_runs_the_checks(self):
        run = ('Cargo.lock', 'Cargo.toml', 'rust-toolchain.toml', 'a/Cargo.toml', 'a/build.rs',
               'a/src/lib.rs', 'crates/grapt-types/src/lib.rs', '.github/workflows/ci.yml',
               '.cargo/config.toml', 'clippy.toml', 'deny.toml', 'about.toml',
               'scripts/package/windows_package.py', 'scripts/release/licenses.py',
               'fixtures/some.bin', 'a/src/unknown.xyz', 'packages/stateengine/x')
        for path in run:
            with self.subTest(path=path):
                self.assertTrue(windows_scope.required({path}))
        # One inert path beside a build input still runs the checks.
        self.assertTrue(windows_scope.required({'docs/guide.md', 'a/src/lib.rs'}))
        # An empty set is an unknown change, not an empty diff.
        self.assertTrue(windows_scope.required(set()))

    def test_a_documentation_batch_through_the_planner_keeps_linux_commands(self):
        with landing_repository() as (root, base):
            (root/'docs').mkdir(exist_ok=True)
            (root/'docs/guide.md').write_text('# Guide\n')
            commit_paths(root, ['docs/guide.md'])
            paths = self.changed(root, base)
            packages = json.loads(subprocess.check_output(
                ['cargo', 'metadata', '--locked', '--no-deps', '--format-version=1'],
                cwd=root, env=isolated_environment()))['packages']
            ordinary = gate_plan.commands('affected', packages, paths, root, base=base)
            batch = gate_plan.commands('affected', packages, paths, root, base=base, batch=True)
            self.assertEqual(batch, [c for c in ordinary if c[:2] != ['cargo', 'xwin']])
            # A docs-only change selects no crate, so no leg is planned either
            # way; batch mode changes nothing about the Linux commands.
            nightly = gate_plan.commands('affected', packages, paths, root, base=base,
                                         batch=True, nightly=True)
            self.assertEqual(any(c[:2] == ['cargo', 'xwin'] for c in ordinary),
                             any(c[:2] == ['cargo', 'xwin'] for c in nightly))

    def test_windows_machinery_changes_run_the_checks(self):
        for path in ('scripts/build/windows_gate.py', 'scripts/build/windows_scope.py',
                     'scripts/build/gate.py', 'scripts/build/gate_plan.py',
                     'scripts/build/receipt.py', 'scripts/build/test_land.ps1',
                     'scripts/build/windows_native.py', 'scripts/build/windows_native.ps1',
                     'scripts/build/xwin_something.py'):
            with self.subTest(path=path):
                self.assertTrue(windows_scope.required({path}))

    def test_a_plain_box_script_may_skip_the_checks(self):
        for path in ('scripts/build/disk_hygiene.py', 'scripts/build/evict_targets.py',
                     'scripts/ci/changes.py', 'scripts/daily-use/x.py'):
            with self.subTest(path=path):
                self.assertFalse(windows_scope.required({path}))

    def test_every_module_the_windows_planner_imports_runs_the_checks(self):
        # Guard the run list against drift: a new local import in the planner is
        # itself a build input, so changing it must not skip its own check.
        import ast
        scripts = Path(__file__).resolve().parent
        planner = scripts/'windows_gate.py'
        imported = set()
        for node in ast.walk(ast.parse(planner.read_text())):
            if isinstance(node, ast.Import):
                imported.update(alias.name.split('.')[0] for alias in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported.add(node.module.split('.')[0])
        local = {name for name in imported if (scripts/f'{name}.py').is_file()}
        self.assertTrue(local, 'the planner imports at least one local module')
        for name in sorted(local):
            with self.subTest(module=name):
                self.assertTrue(windows_scope.required({f'scripts/build/{name}.py'}),
                                f'{name} is imported by the Windows planner but would skip it')

    def test_full_batch_receipt_verifies_its_actual_plan_and_rejects_policy_tampering(self):
        with landing_repository() as (root, base), chdir(root), \
                patch.dict('os.environ', isolated_environment()):
            (root/'a/src/lib.rs').write_text('pub fn changed() {}\n')
            commit = commit_paths(root, ['a/src/lib.rs'])
            packages = json.loads(subprocess.check_output(
                ['cargo', 'metadata', '--locked', '--no-deps', '--format-version=1']))['packages']
            commands = gate_plan.commands('full', packages, self.changed(root, base), root,
                                          base=base, batch=True)
            log = root/'gate.log'
            log.write_text('# secode-gate-run 0123456789abcdef0123456789abcdef\nAll selected commands exited zero\n')
            source = {'commit': commit, 'clean': True}
            with patch.object(receipt, 'source', return_value=source):
                receipt.write(log, 'full', source, commands, 1, base=base, batch=True,
                              nextest_junit=nextest_result(log, commands))
            receipt.verify(log, commit, base=base, batch=True)
            with self.assertRaisesRegex(ValueError, 'batch policy'):
                receipt.verify(log, commit, base=base)
            path = log.with_suffix('.log.receipt.json')
            record = json.loads(path.read_text())
            record['batch'] = False
            path.write_text(json.dumps(record))
            with self.assertRaisesRegex(ValueError, 'batch policy'):
                receipt.verify(log, commit, base=base, batch=True)
