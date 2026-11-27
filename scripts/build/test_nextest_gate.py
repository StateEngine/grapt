"""The isolated runner keeps ordinary, ignored, exact-skip and doctest coverage."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

from fixture_environment import isolated_environment
import nextest_gate
import gate_plan
import test_features


class NextestTests(unittest.TestCase):
    def test_real_runner_preserves_scope_and_retains_success_and_failure_reports(self):
        with tempfile.TemporaryDirectory(prefix='secode-nextest-test-') as directory:
            root=Path(directory); (root/'src').mkdir()
            (root/'Cargo.toml').write_text('[package]\nname="nextest-gate-fixture"\nversion="0.1.0"\nedition="2021"\n')
            source='''/// ```
/// assert_eq!(nextest_gate_fixture::value(), 1);
/// ```
pub fn value() -> u32 { 1 }
#[test] fn ordinary() { assert_eq!(value(), 1); }
#[test] #[ignore] fn ignored() { panic!("ignored child must stay ignored"); }
mod tagged {
    #[test] fn slow() { panic!("nightly-only fixture must stay excluded"); }
    #[test] fn slow_extra() {}
}
'''
            (root/'src/lib.rs').write_text(source)
            env=dict(isolated_environment(),CARGO_TARGET_DIR=str(root/'target'))
            subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,env=env,check=True,capture_output=True,timeout=20)
            original=['cargo','test','--locked','-p','nextest-gate-fixture','--no-fail-fast',
                      '--','--test-threads=1','--exact','--skip','tagged::slow']
            planned=nextest_gate.plan([original])
            for failure in (False,True):
                if failure:
                    (root/'src/lib.rs').write_text(source.replace('assert_eq!(value(), 1);','panic!("ordinary failure");'))
                path=root/('failure.log' if failure else 'success.log')
                with path.open('w') as log:
                    with nextest_gate.invocation(planned[0],log) as command:
                        outcome=subprocess.run(command,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT,timeout=60)
                    if not failure:
                        subprocess.run(planned[1],cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=60)
                self.assertEqual(outcome.returncode==0,not failure)
                if failure:
                    self.assertIn('# nextest failure:',path.read_text())
                    with self.assertRaisesRegex(ValueError,'failing tests'):nextest_gate.verify(path)
                else:
                    cases=ET.fromstring(nextest_gate.read_report(path)).findall('.//testcase')
                    ran={case.get('name') for case in cases if case.find('skipped') is None}
                    self.assertEqual(ran,{'ordinary','tagged::slow_extra'})
                    digest=nextest_gate.verify(path)
                    report=nextest_gate.report(path);report.write_bytes(report.read_bytes()+b'\n')
                    with self.assertRaisesRegex(ValueError,'changed'):nextest_gate.verify(path,digest)
                    self.assertIn('Doc-tests',path.read_text())

    def test_targeted_integration_checks_keep_their_existing_cargo_runner(self):
        commands=[['cargo','test','--locked','-p','grapt-product','--test','handover_journey',
                   'ordinary_binary_ignores_the_fixture','--no-fail-fast','--','--test-threads=4']]
        self.assertEqual(nextest_gate.plan(commands),commands)

    def test_binary_only_and_mixed_scopes_keep_only_real_library_doctests(self):
        with tempfile.TemporaryDirectory(prefix='secode-nextest-binary-') as directory:
            root=Path(directory)
            (root/'Cargo.toml').write_text('[workspace]\nmembers=["app","library"]\nresolver="2"\n')
            for package in ('app','library'):
                (root/package/'src').mkdir(parents=True)
                (root/package/'Cargo.toml').write_text(
                    f'[package]\nname="{package}"\nversion="0.1.0"\nedition="2021"\n[features]\nspecial=[]\n')
            (root/'app/src/main.rs').write_text('fn main() {}\n#[test] fn ordinary() { assert!(cfg!(feature="special")); }\n')
            library=root/'library/src/lib.rs'
            source='/// ```\n/// assert_eq!(library::value(), 2);\n/// ```\npub fn value() -> u32 { if cfg!(feature="special") { 2 } else { 1 } }\n#[test] fn ordinary() {}\n'
            library.write_text(source)
            env=dict(isolated_environment(),CARGO_TARGET_DIR=str(root/'target'))
            subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,env=env,check=True,capture_output=True,timeout=20)
            packages=json.loads(subprocess.check_output(['cargo','metadata','--locked','--no-deps','--format-version','1'],cwd=root,env=env))['packages']
            with patch.dict(test_features.FEATURES,{'app':('special',),'library':('special',)}):
                for mixed in (False,True):
                    paths={'app/src/main.rs'} | ({'library/src/lib.rs'} if mixed else set())
                    commands=[row for row in gate_plan.commands('affected',packages,paths,root)
                              if row[:3]==['cargo','nextest','run'] or '--doc' in row]
                    log_path=root/('mixed.log' if mixed else 'binary.log')
                    with log_path.open('w') as log:
                        for command in commands:
                            with nextest_gate.invocation(command,log) as invocation:
                                subprocess.run(invocation,cwd=root,env=env,stdout=log,stderr=subprocess.STDOUT,check=True,timeout=60)
                    nextest_gate.verify(log_path)
                library.write_text(source.replace('value(), 2','value(), 3'))
                with self.assertRaises(subprocess.CalledProcessError):
                    subprocess.run(next(row for row in commands if '--doc' in row),cwd=root,env=env,check=True,capture_output=True,timeout=60)
