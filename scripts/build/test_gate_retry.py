"""Real Cargo/libtest failures, isolated retries and receipt rejection probes."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import subprocess
import shutil
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import gate_retry
import gate_plan
from fixture_environment import isolated_environment, nextest_result


FLAKE = '''#[test]
fn flaky_once() {
    match std::fs::OpenOptions::new().write(true).create_new(true).open("seen") {
        Ok(_) => panic!("injected first failure"),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(e) => panic!("unexpected marker failure: {e}"),
    }
}
'''


@contextmanager
def fixture():
    with tempfile.TemporaryDirectory(prefix='secode-retry-test-') as directory:
        root = Path(directory)
        (root/'Cargo.toml').write_text('[workspace]\nmembers=["a","b"]\nresolver="2"\n')
        for name in ('a','b'):
            (root/name/'src').mkdir(parents=True)
            (root/name/'Cargo.toml').write_text(f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2021"\n')
            (root/name/'src/lib.rs').write_text('pub fn value() {}\n')
        with (root/'b/src/lib.rs').open('a') as output:
            output.write(FLAKE+'#[test]\nfn ordinary_pass() {}\n')
        env = {k:v for k,v in isolated_environment().items()
               if not k.startswith(('SECODE_','CARGO_','RUSTC_','RUSTFLAGS'))}
        env['CARGO_TARGET_DIR'] = str(root/'target')
        with patch.dict(os.environ,env,clear=True):
            subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,check=True,capture_output=True)
            metadata = json.loads(subprocess.check_output(
                ['cargo','metadata','--locked','--no-deps','--format-version','1'],cwd=root))
            yield root,metadata['packages']


def execute(command,log,deadline,**options):
    began = time.monotonic()
    subprocess.run(command,cwd=options['cwd'],stdout=log,stderr=subprocess.STDOUT,
                   check=True,timeout=max(0.01,deadline-began))
    log.flush()
    return time.monotonic()-began


COMMAND = ['cargo','test','--locked','--workspace','--no-fail-fast','--','--test-threads=4']


def create_gate_fixture(root):
    """Tiny real gate policy/receipt, with fixture-only cache and license inputs."""
    root.mkdir()
    build = root/'scripts/build'; build.mkdir(parents=True)
    for path in Path(gate_retry.__file__).parent.glob('*.py'):
        if not path.name.startswith('test_') or path.name == 'test_features.py':
            shutil.copyfile(path,build/path.name)
    source = Path(gate_retry.__file__).resolve().parents[2]
    shutil.copyfile(source/'scripts/land.ps1',root/'scripts/land.ps1')
    # Keep the full landing door in the test while isolating host-wide controls.
    heavy = build/'heavy.py'
    heavy.write_text(heavy.read_text().replace("Path.home()/'secode-heavy.lock'",repr(root.parent/'heavy.lock'))
                     .replace("Path.home()/'.local/state/secode/heavy'",repr(root.parent/'queue'))
                     .replace('PosixPath(', 'Path('))
    with (build/'ram_targets.py').open('a') as output:
        output.write('\ndef allocate(*a,**k):\n    raise ValueError("tiny fixture stays exclusive")\n')
    (root/'scripts/release').mkdir()
    (root/'scripts/release/licenses.py').write_text('# Tiny fixture has no external dependencies.\n')
    (build/'test_fixture.py').write_text('''import unittest
from pathlib import Path
class FixtureTests(unittest.TestCase):
    def test_lockfile_exists(self):
        self.assertTrue(Path("Cargo.lock").is_file())
''')
    for rel, doc in (
        ('crates/grapt-types/tests/include-ratchet.json', {'carriers': {}}),
        ('crates/grapt-types/tests/legacy-expect-ratchet.json', {'files': {}}),
        ('scripts/build/module-cycles.json', {'deep': [], 'configs': [], 'cycles': {}, 'edges': {}}),
    ):
        dest = root / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(json.dumps(doc) + '\n')
    (root/'scripts/build-cache.sh').write_text(
        f'export CARGO_TARGET_DIR={root}/target\nexport SECODE_TARGET_ROOT={root}/target-root\n'
        f'export SECODE_FIXTURES={root.parent}/fixtures\n'
        'unset RUSTC_WRAPPER RUSTFLAGS CARGO_ENCODED_RUSTFLAGS\n')
    (build/'disk_hygiene.py').write_text('# Fixture owns no host cache entries.\n')
    (root/'scripts/shared-cargo.sh').write_text('exec cargo "$@"\n')
    (root/'scripts/gate.sh').write_text('''source scripts/build-cache.sh
exec python3 -c 'import sys,tempfile; sys.path.insert(0,"scripts/build"); import gate; gate.evict_targets.prepare=lambda *a, **k: tempfile.TemporaryFile(); gate.evict_targets.inherit=lambda *a, **k: None; gate.evict_targets.retain=lambda *a: None; sys.exit(gate.main())' "$@"
''')
    (root/'.gitignore').write_text('target/\nseen\n__pycache__/\n')
    (root/'README.md').write_text('base\n')
    (root/'Cargo.toml').write_text('[workspace]\nmembers=["grapt-types","grapt-store"]\nresolver="2"\n')
    for package,names in [('grapt-types',['no_god_files','direct_include_format','crate_graph']),
                          ('grapt-store',['write_chokepoint'])]:
        (root/package/'src').mkdir(parents=True)
        (root/package/'tests').mkdir()
        (root/package/'Cargo.toml').write_text(f'[package]\nname="{package}"\nversion="0.1.0"\nedition="2021"\n')
        (root/package/'src/lib.rs').write_text('pub fn value() {}\n')
        for name in names:
            (root/package/'tests'/f'{name}.rs').write_text(
                FLAKE if name == 'no_god_files' else '#[test]\nfn passes() {}\n')
    env = {k:v for k,v in isolated_environment().items() if not k.startswith(('SECODE_','CARGO_','RUSTC_','RUSTFLAGS'))}
    env.update(CARGO_TARGET_DIR=str(root/'target'),SECODE_TARGET_ROOT=str(root/'target-root'),
               SECODE_FIXTURES=str(root.parent/'fixtures'),PYTHONDONTWRITEBYTECODE='1')
    (root.parent/'fixtures').mkdir(exist_ok=True)
    def git(*args,**kwargs):
        return subprocess.check_output(['git',*args],cwd=root,**kwargs).decode().strip()
    git('init','-q','-b','main')
    git('config','user.name','Fixture Author');git('config','user.email','fixture@users.noreply.github.com')
    subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,env=env,check=True,capture_output=True)
    paths = [p.relative_to(root).as_posix() for p in root.rglob('*') if p.is_file() and '.git' not in p.relative_to(root).parts]
    git('add','--',*paths)
    git('commit','-q','-F','-','--',*paths,input=(b'chore(fixture): tiny retry fixture\n\n'
                                                  b'Co-Authored-By: Codex <noreply@openai.com>\n'))
    base = git('rev-parse','HEAD')
    git('checkout','-q','-b','retry-witness')
    (root/'README.md').write_text('docs change leaves test binaries out of affected set\n')
    git('add','--','README.md');git('commit','-q','-F','-','--','README.md',input=(b'docs(fixture): docs fixture\n\n'
                                                                                   b'Co-Authored-By: Codex <noreply@openai.com>\n'))
    return env,base,git('rev-parse','HEAD')


class RetryTests(unittest.TestCase):
    def test_oversized_log_is_refused_before_any_open_or_read(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root/'oversized.log'
            with log.open('wb') as output:output.truncate(gate_retry.MAX_LOG+1)
            with patch.object(gate_retry.os,'open',side_effect=AssertionError('opened oversized log')), \
                    patch.object(Path,'read_bytes',side_effect=AssertionError('read oversized log')):
                with self.assertRaisesRegex(ValueError,'within 64 MB'):
                    gate_retry.verify([{}],[],log,[],[],root)

    def run_case(self,root,packages,affected):
        path = root/'gate.log'
        with path.open('w') as log:
            return gate_retry.run(COMMAND,log,time.monotonic()+30,execute=execute,
                                  packages=packages,affected=affected,index=0,cwd=root)

    def test_out_of_set_flake_retries_only_the_failed_test_and_keeps_original_output(self):
        with fixture() as (root,packages):
            seconds,record = self.run_case(root,packages,['a'])
            self.assertGreater(seconds,0)
            self.assertEqual(record['affected'],['a'])
            self.assertEqual([f['package'] for f in record['failures']],['b'])
            self.assertEqual(record['failures'][0]['tests'],['flaky_once'])
            self.assertEqual(record['retries'][0]['test'],'flaky_once')
            log = (root/'gate.log').read_text()
            self.assertIn('injected first failure',log)
            self.assertIn('test result: FAILED.',log)
            self.assertIn('1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out',log)
            gate_retry.verify([record],[COMMAND],root/'gate.log',packages,['a'],root)
            with (root/'gate.log').open('a') as output:
                output.write('+ isolated retry: b::b::flaky_once\n')
            with self.assertRaisesRegex(ValueError,'Retry markers'):
                gate_retry.verify([record],[COMMAND],root/'gate.log',packages,['a'],root)

    def test_workspace_features_retry_executes_the_same_selected_binary(self):
        with fixture() as (root,packages):
            for package,feature in [('a','unrelated'),('b','required')]:
                with (root/package/'Cargo.toml').open('a') as output:
                    output.write(f'[features]\n{feature}=[]\n')
            source=root/'b/src/lib.rs'
            source.write_text(source.read_text().replace('fn flaky_once() {',
                'fn flaky_once() { assert!(cfg!(feature="required"));'))
            command=COMMAND[:5]+['--features','a/unrelated,b/required',*COMMAND[5:]]
            log=root/'gate.log'
            with log.open('w') as output:
                _,record=gate_retry.run(command,output,time.monotonic()+30,execute=execute,
                    packages=packages,affected=['a'],index=0,cwd=root)
            retry=record['retries'][0]['command']
            self.assertIn('required',retry)
            self.assertNotIn('a/unrelated,b/required',retry)
            self.assertEqual(record['failures'][0]['features'],['required'])
            self.assertIn('test flaky_once ... ok',log.read_text())
            gate_retry.verify([record],[command],log,packages,['a'],root)

    def test_the_same_flake_in_the_affected_set_never_retries(self):
        with fixture() as (root,packages):
            with self.assertRaises(subprocess.CalledProcessError):
                self.run_case(root,packages,['b'])
            self.assertNotIn('isolated retry:',(root/'gate.log').read_text())

    def test_second_failure_stays_red_and_is_not_retried_twice(self):
        with fixture() as (root,packages):
            path = root/'b/src/lib.rs'
            path.write_text('#[test]\nfn always_fails() { panic!("still red"); }\n')
            with self.assertRaises(subprocess.CalledProcessError):
                self.run_case(root,packages,[])
            log = (root/'gate.log').read_text()
            self.assertEqual(log.count('+ isolated retry:'),1)
            self.assertEqual(log.count('test result: FAILED.'),2)

    def test_exhausted_gate_retry_limit_stays_red_before_starting_a_retry(self):
        with fixture() as (root,packages):
            with (root/'gate.log').open('w') as log,self.assertRaises(subprocess.CalledProcessError):
                gate_retry.run(COMMAND,log,time.monotonic()+30,execute=execute,packages=packages,
                               affected=[],index=0,cwd=root,retry_budget=0)
            self.assertNotIn('isolated retry:',(root/'gate.log').read_text())

    def test_selected_package_feature_unification_is_restored(self):
        with fixture() as (root,packages):
            with (root/'a/Cargo.toml').open('a') as output:
                output.write('[dependencies]\nb={path="../b",features=["from_a"]}\n')
            with (root/'b/Cargo.toml').open('a') as output:
                output.write('[features]\nfrom_a=[]\n')
            subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,check=True,capture_output=True)
            _,record=self.run_case(root,packages,[])
            self.assertEqual(record['failures'][0]['features'],['from_a'])
            gate_retry.verify([record],[COMMAND],root/'gate.log',packages,[],root)

    def test_changed_dependency_unification_cannot_qualify_another_binary(self):
        with fixture() as (root,packages):
            (root/'c/src').mkdir(parents=True)
            (root/'c/Cargo.toml').write_text('[package]\nname="c"\nversion="0.1.0"\nedition="2021"\n[features]\nfrom_a=[]\n')
            (root/'c/src/lib.rs').write_text('pub fn value() -> bool { cfg!(feature="from_a") }\n')
            (root/'Cargo.toml').write_text('[workspace]\nmembers=["a","b","c"]\nresolver="2"\n')
            for package,features in [('a',',features=["from_a"]'),('b','')]:
                with (root/package/'Cargo.toml').open('a') as output:
                    output.write('[dependencies]\nc={path="../c"'+features+'}\n')
            subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,check=True,capture_output=True)
            packages=json.loads(subprocess.check_output(['cargo','metadata','--locked','--no-deps','--format-version','1'],cwd=root))['packages']
            with self.assertRaisesRegex(ValueError,'original binary'):
                self.run_case(root,packages,[])

    def test_unknown_build_or_incomplete_failure_output_cannot_retry(self):
        with fixture() as (root,packages):
            for output in (b'error: could not compile b\n',b'test arbitrary ... FAILED\n',
                           b'{"reason":"build-finished","success":true}\n'):
                with self.subTest(output=output),self.assertRaises(ValueError):
                    gate_retry.failed_tests(output,packages,root)

    def test_receipt_cannot_reclassify_an_in_set_retry_or_change_failure_output(self):
        with fixture() as (root,packages):
            _,record = self.run_case(root,packages,[])
            record['affected'] = ['b']
            with self.assertRaisesRegex(ValueError,'affected test'):
                gate_retry.verify([record],[COMMAND],root/'gate.log',packages,['b'],root)
            record['affected'] = []
            with (root/'gate.log').open('r+b') as log:
                log.seek(record['original']['offset'])
                log.write(b'!')
            with self.assertRaisesRegex(ValueError,'log changed'):
                gate_retry.verify([record],[COMMAND],root/'gate.log',packages,[],root)

    def test_real_gate_writes_a_reusable_pass_with_retry_receipt(self):
        with tempfile.TemporaryDirectory(prefix='secode-retry-door-') as directory:
            outer = Path(directory)
            root = outer/'candidate'
            env,base,commit = create_gate_fixture(root)
            log = outer/'gate.log'
            result = subprocess.run(['bash','scripts/gate.sh','affected','--base',base,'--log',str(log)],
                                    cwd=root,env=env,capture_output=True,text=True,timeout=45)
            self.assertEqual(result.returncode,0,result.stdout+result.stderr+log.read_text())
            self.assertIn('PASS_WITH_RETRY affected',result.stdout)
            record = json.loads(log.with_suffix('.log.receipt.json').read_text())
            self.assertEqual(record['status'],'PASS_WITH_RETRY')
            verifier = [sys.executable,'scripts/build/receipt.py',str(log),commit,'--base',base]
            verified = subprocess.run(verifier,cwd=root,env=env,capture_output=True,text=True,timeout=15)
            self.assertEqual(verified.returncode,0,verified.stdout+verified.stderr)
            downgraded = dict(record,status='PASS')
            del downgraded['retries']
            log.with_suffix('.log.receipt.json').write_text(json.dumps(downgraded))
            refused = subprocess.run(verifier,cwd=root,env=env,capture_output=True,text=True,timeout=15)
            self.assertNotEqual(refused.returncode,0)
            self.assertIn('PASS_WITH_RETRY evidence',refused.stderr)
            log.with_suffix('.log.receipt.json').write_text(json.dumps(record))
            original = log.read_bytes()
            with log.open('a') as output:output.write('doctored log\n')
            self.assertNotEqual(subprocess.run(verifier,cwd=root,env=env,capture_output=True,timeout=15).returncode,0)
            log.write_bytes(original)
            # Adversarial receipt input: even forged source/coverage fields
            # cannot make a retry of a now-affected crate pass verification.
            path = root/'grapt-types/src/lib.rs'
            path.write_text('pub fn changed() {}\n')
            subprocess.run(['git','add','--','grapt-types/src/lib.rs'],cwd=root,check=True)
            subprocess.run(['git','commit','-q','-F','-','--','grapt-types/src/lib.rs'],
                           cwd=root,input=b'In-set rejection fixture\n',check=True)
            inset = subprocess.check_output(['git','rev-parse','HEAD'],cwd=root,text=True).strip()
            packages = json.loads(subprocess.check_output(
                ['cargo','metadata','--locked','--no-deps','--format-version','1'],cwd=root,env=env))['packages']
            commands = gate_plan.commands('affected',packages,{'README.md','grapt-types/src/lib.rs'},root,base=base)
            original_command = record['commands'][record['retries'][0]['command_index']]
            record['retries'][0]['command_index'] = len(commands)
            commands.append(original_command)
            record.update(commands=commands,source_before={'commit':inset,'clean':True},
                          source_after={'commit':inset,'clean':True})
            record['nextest_junit'] = nextest_result(log,commands)
            record['retries'][0]['affected'] = ['grapt-types']
            log.with_suffix('.log.receipt.json').write_text(json.dumps(record))
            verifier[3] = inset
            refused = subprocess.run(verifier,cwd=root,env=env,capture_output=True,text=True,timeout=15)
            self.assertNotEqual(refused.returncode,0)
            self.assertIn('affected test',refused.stderr)


class NestedSummaryTests(unittest.TestCase):
    """An inherited-stdio libtest child cannot finish its parent's Cargo artifact."""
    def parse(self, body):
        root = Path('/tmp/secode-nested-summary-fixture')
        target = {'name':'fixture','kind':['lib'],'src_path':str(root/'src/lib.rs')}
        package = {'id':'fixture-id','name':'fixture','targets':[target]}
        artifact = {'reason':'compiler-artifact','package_id':package['id'],
                    'profile':{'test':True},'target':target,'features':[],
                    'executable':str(root/'target/debug/deps/fixture')}
        raw = '\n'.join([json.dumps(artifact),
            '{"reason":"build-finished","success":true}',
            '     Running unittests src/lib.rs (target/debug/deps/fixture)', *body, '']).encode()
        return gate_retry.failed_tests(raw,[package],root)

    def test_nested_success_after_an_outer_failure_does_not_close_the_parent(self):
        result = self.parse([
            'running 3 tests', 'test outer_failure ... FAILED',
            'test observer ... ', 'running 1 test', 'test ceiling_child ... ok',
            'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1102 filtered out;',
            'ok', 'test ordinary ... ok', 'failures:', '    outer_failure',
            'test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;',
            'error: test failed, to rerun pass `-p fixture --lib`',
        ])
        self.assertEqual(len(result),1)
        self.assertEqual(result[0]['tests'],['outer_failure'])
        self.assertEqual(result[0]['executable'],'/tmp/secode-nested-summary-fixture/target/debug/deps/fixture')

    def test_a_failed_child_does_not_lend_its_name_to_the_outer_failure(self):
        result = self.parse([
            'running 1 test','test parent ... ','running 1 test',
            'test child ... FAILED','failures:','    child',
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;',
            'FAILED','failures:','    parent',
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;',
        ])
        self.assertEqual(result[0]['tests'],['parent'])

    def test_nested_output_in_captured_failure_stdout_keeps_parent_identity(self):
        result = self.parse([
            'running 1 test','test parent ... FAILED','failures:',
            '---- parent stdout ----','running 1 test','test child ... ok',
            'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;',
            'failures:','    parent',
            'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;',
        ])
        self.assertEqual(result[0]['tests'],['parent'])

    def test_missing_outer_summary_and_wrong_child_counts_remain_red(self):
        cases = [
            ['running 1 test','test parent ... ','running 1 test','test child ... ok',
             'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;','ok'],
            ['running 1 test','test parent ... ','running 1 test','test child ... FAILED',
             'test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out;'],
            ['running 1 test','running 1 test','test child ... FAILED',
             'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;'],
        ]
        for body in cases:
            with self.subTest(body=body),self.assertRaises(ValueError):
                self.parse(body)

    def test_a_crash_writer_child_can_exit_without_its_own_summary(self):
        result = self.parse([
            'running 2 tests','test outer_failure ... FAILED',
            'test crash_recovery ... ','running 1 test','ok',
            'failures:','    outer_failure',
            'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;',
        ])
        self.assertEqual(result[0]['tests'],['outer_failure'])

    def test_nesting_is_bounded_before_a_retry_can_be_admitted(self):
        body=['running 1 test']
        for level in range(17):
            body.extend([f'test parent_{level} ... ','running 1 test'])
        with self.assertRaisesRegex(ValueError,'bounded parent context'):
            self.parse(body)

    def test_a_failed_child_without_its_summary_cannot_disappear(self):
        with self.assertRaises(ValueError):
            self.parse([
                'running 2 tests','test outer_failure ... FAILED',
                'test parent ... ','running 1 test','test child ... FAILED','ok',
                'failures:','    outer_failure',
                'test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;',
            ])
