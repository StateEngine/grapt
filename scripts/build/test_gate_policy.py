"""Gate cost follows the reviewed paths, including real lockfile disambiguation."""
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import gate_policy
import gate_plan
import slow_tests
import receipt
from test_receipt import landing_repository, commit_paths, verify_candidate
from fixture_environment import isolated_environment


class GatePolicyTests(unittest.TestCase):
    def test_full_exceptions_and_ordinary_crates_are_distinct(self):
        for crate in gate_policy.CORE:
            self.assertEqual(gate_policy.required_mode({f'crates/{crate}/src/lib.rs'}), 'full')
        for path in ['scripts/build/gate.py', 'scripts/land.ps1', 'scripts/gate.sh',
                     'scripts/release/package.py', 'scripts/package/build_daily.sh', 'Cargo.toml']:
            self.assertEqual(gate_policy.required_mode({path}), 'full', path)
        for path in ['crates/grapt-client/src/lib.rs', 'crates/grapt-api/src/http1.rs',
                     'docs/BUILD-AND-LAND.md', 'scripts/building-a-feature.md']:
            self.assertEqual(gate_policy.required_mode({path}), 'affected', path)
        self.assertEqual(gate_policy.required_mode(set(), row_a=True), 'full')

    def test_old_or_tier2_receipt_cannot_bypass_a_core_or_build_path(self):
        for path in ['crates/grapt-runtime/src/probe.rs', 'scripts/build/probe.py']:
            with self.subTest(path=path), landing_repository() as (root, base):
                file = root/path; file.parent.mkdir(parents=True); file.write_text('fixture\n')
                candidate = commit_paths(root, [path])
                rejected = verify_candidate(root, base, candidate)
                self.assertNotEqual(rejected.returncode, 0)
                self.assertIn('Changed paths require a full', rejected.stderr)
                accepted = verify_candidate(root, base, candidate, mode='full')
                self.assertEqual(accepted.returncode, 0, accepted.stderr)

    def test_nightly_runs_tagged_names_and_landing_retains_ordinary_tests(self):
        landing = gate_plan.test_command(['grapt-product'])
        nightly = gate_plan.test_command(['grapt-product'], nightly=True)
        self.assertIn('--skip', landing)
        self.assertNotIn('--skip', nightly)
        self.assertNotIn('--ignored', nightly)
        self.assertNotIn('--include-ignored', nightly)
        self.assertEqual([landing[index + 1] for index, arg in enumerate(landing[:-1]) if arg == '--skip'],
                         list(slow_tests.TESTS['grapt-product']))
        self.assertNotIn('--skip', gate_plan.test_command(['grapt-api']))

    def test_a_renamed_nightly_test_fails_instead_of_silently_disappearing(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            with patch.dict(slow_tests.TESTS, {'sample': {'module::missing': 'test.rs'}}, clear=True):
                with self.assertRaisesRegex(ValueError, 'source is missing'):
                    slow_tests.validate(root)
                (root/'test.rs').write_text('#[test]\nfn renamed() {}\n')
                with self.assertRaisesRegex(ValueError, 'does not identify one'):
                    slow_tests.validate(root)
                source = '#[tokio::test(flavor = "multi_thread", worker_threads = 1)]\nasync fn missing() {}\n'
                (root/'test.rs').write_text(source)
                slow_tests.validate(root)
                (root/'test.rs').write_text(source+source)
                with self.assertRaisesRegex(ValueError, 'does not identify one'):
                    slow_tests.validate(root)
                marked = '#[test]\n#[expect(clippy::too_many_lines, reason = "legacy")]\nfn missing(\n) {}\n'
                (root/'test.rs').write_text(marked)
                slow_tests.validate(root)

    def test_equivalent_lock_edges_remain_scoped_but_actual_resolution_changes_do_not(self):
        with landing_repository() as (root, _base):
            manifest=root/'Cargo.toml'
            manifest.write_text(manifest.read_text()+'exclude=["vendor/new","vendor/old"]\n')
            # Two local versions produce Cargo's actual abbreviated/disambiguated edge syntax.
            for name, version in [('new','0.22.1'), ('old','0.21.7')]:
                folder=root/'vendor'/name; (folder/'src').mkdir(parents=True)
                (folder/'Cargo.toml').write_text(f'[package]\nname="base64"\nversion="{version}"\nedition="2021"\n')
                (folder/'src/lib.rs').write_text('pub fn fixture() {}\n')
            package=root/'a/Cargo.toml'
            package.write_text(package.read_text()+'base64={path="../vendor/new"}\n')
            environment=isolated_environment()
            def lock():
                subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,env=environment,
                               check=True,capture_output=True,timeout=10)
            lock()
            base=commit_paths(root,['Cargo.toml','Cargo.lock','a/Cargo.toml',
                                    'vendor/new/Cargo.toml','vendor/new/src/lib.rs',
                                    'vendor/old/Cargo.toml','vendor/old/src/lib.rs'])
            package.write_text(package.read_text()+'compat={package="base64",path="../vendor/old"}\n')
            lock()
            paths={'Cargo.lock','a/Cargo.toml'}
            self.assertIn('base64 0.22.1', (root/'Cargo.lock').read_text())
            self.assertEqual(gate_plan.scoped_paths(paths, root, base), {'a/Cargo.toml'})
            self.assertEqual(gate_policy.required_mode(paths, root=root, base=base), 'affected')
            package.write_text(package.read_text().replace('base64={path="../vendor/new"}',
                                                           'base64={path="../vendor/old"}'))
            package.write_text(package.read_text().replace('compat={package="base64",path="../vendor/old"}\n',''))
            lock()
            self.assertEqual(gate_plan.scoped_paths(paths, root, base), paths)
            self.assertEqual(gate_policy.required_mode(paths, root=root, base=base), 'full')

    def test_exact_nightly_tags_keep_an_untagged_name_with_the_same_prefix(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            name='nightly_case'
            source=root/'tests.rs'
            source.write_text('mod fixture {\n'+
                f'#[test] fn {name}() {{}}\n'+
                f'#[test] fn {name}_ordinary_regression() {{}}\n'+
                '}\n')
            binary=root/'tests'
            subprocess.run(['rustc','--test','--edition=2021',str(source),'-o',str(binary)],
                           check=True,capture_output=True,timeout=20)
            with patch.dict(slow_tests.TESTS, {'fixture': {'fixture::'+name: 'fixture.rs'}}, clear=True):
                arguments=slow_tests.arguments(['fixture'])
            old=subprocess.run([str(binary),*[arg for arg in arguments if arg!='--exact']],
                               check=True,capture_output=True,text=True,timeout=5)
            self.assertIn('0 passed;',old.stdout)
            corrected=subprocess.run([str(binary),*arguments],check=True,capture_output=True,text=True,timeout=5)
            self.assertIn('1 passed;',corrected.stdout)
            self.assertIn(name+'_ordinary_regression ... ok',corrected.stdout)
