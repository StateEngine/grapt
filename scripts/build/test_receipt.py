import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager, chdir
from unittest.mock import patch

import receipt
import gate
from fixture_environment import isolated_environment as fixture_environment, nextest_result



@contextmanager
def landing_repository():
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        subprocess.run(['git', 'init', '-q', '-b', 'main', str(root)], check=True)
        (root/'README.md').write_text('base\n')
        (root/'Cargo.toml').write_text('[workspace]\nmembers=["a", "b", "c"]\nresolver="2"\n')
        paths = ['README.md', 'Cargo.toml']
        for name, dependencies in [('a', ''), ('b', 'a={path="../a"}\n'), ('c', 'b={path="../b"}\n')]:
            (root/name/'src').mkdir(parents=True)
            (root/name/'Cargo.toml').write_text(f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2021"\n[dependencies]\n{dependencies}')
            (root/name/'src/lib.rs').write_text('pub fn value() {}\n')
            paths.extend([name+'/Cargo.toml', name+'/src/lib.rs'])
        subprocess.run(['cargo', 'generate-lockfile', '--offline'], cwd=root, check=True, capture_output=True, env=fixture_environment())
        paths.append('Cargo.lock')
        commit_paths(root, paths)
        base = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root).decode().strip()
        yield root, base


def commit_paths(root, paths):
    subprocess.run(['git', 'add', '--', *paths], cwd=root, check=True)
    subprocess.run(['git', '-c', 'user.name=Gate', '-c', 'user.email=gate@localhost',
                    'commit', '-q', '-F', '-', '--', *paths], cwd=root,
                   input=b'Owned receipt fixture\n', check=True)
    return subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root).decode().strip()


def workspace_test(commands):
    selected = [command for command in commands
                if command[:2] == ['cargo', 'test'] and '--workspace' in command]
    if len(selected) != 1:
        raise AssertionError('Expected exactly one workspace test command')
    return selected[0]


def verify_candidate(root, base, commit, mode='affected', recorded_base=None, bench_checks=False, require_full=False, omit=None, verify_base=True, native=False):
    log = root/'gate.log'
    log.write_text('# secode-gate-run 0123456789abcdef0123456789abcdef\nall selected commands exited zero\n', newline="\n")
    source = {'commit': commit, 'clean': True}
    paths = {path for path in subprocess.check_output(['git', 'diff', '--no-renames', '--name-only', base, commit], cwd=root, text=True).splitlines()}
    metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'], cwd=root, env=fixture_environment()))
    commands = gate.gate_plan.commands(mode, metadata['packages'], paths, root, base=base)
    if not bench_checks:
        commands = [command for command in commands if 'bench' not in command]
    if native:
        commands = [command for command in commands if command[:2] != ["cargo", "xwin"]]
    if omit:
        commands = [command for command in commands if not omit(command)]
    with patch.object(receipt, 'source', return_value=source):
        receipt.write(log, mode, source, commands, 1, base=base if recorded_base is None else recorded_base,
                      native_windows=log.name+".windows.json" if native else None, nextest_junit=nextest_result(log,commands))
    return subprocess.run([sys.executable, str(Path(receipt.__file__).resolve()), str(log), commit,
                           *(['--base', base] if verify_base else []),
                           *(['--require-full'] if require_full else [])], cwd=root, capture_output=True, text=True, timeout=30, env=fixture_environment())


class ReceiptTests(unittest.TestCase):
    def test_native_receipt_replaces_cross_checks_only_for_the_exact_gate_scope(self):
        import hashlib
        import time
        import windows_gate
        from windows_native_result import SCHEMA
        with landing_repository() as (root, base):
            (root/'a/src/lib.rs').write_text('pub fn changed() {}\n')
            commit=commit_paths(root,['a/src/lib.rs'])
            native=root/'gate.log.windows.json'
            raw=b'Native qualification fixture\n'
            native.with_suffix('.json.log').write_bytes(raw)
            row=dict(schema=SCHEMA,exit=0,source_before=dict(commit=commit,clean=True),
                source_after=dict(commit=commit,clean=True),target=windows_gate.TARGET,
                packages=['a','b'],workspace=False,nightly=False,jobs=2,children_clean=True,
                commands=windows_gate.native_commands(['a','b']),finished_unix=time.time(),
                log_sha256=hashlib.sha256(raw).hexdigest())
            native.write_text(json.dumps(row))
            result=verify_candidate(root,base,commit,native=True)
            self.assertEqual(result.returncode,0,result.stderr)
            row['source_after']['commit']='b'*40; native.write_text(json.dumps(row))
            result=verify_candidate(root,base,commit,native=True)
            self.assertNotEqual(result.returncode,0)
            self.assertIn('clean source and scope',result.stderr)
            native.unlink()
            result=verify_candidate(root,base,commit,native=True)
            self.assertNotEqual(result.returncode,0)
            self.assertIn('missing, linked or oversized',result.stderr)

    def test_full_verification_refuses_unknown_scope_even_when_record_claims_a_base(self):
        with landing_repository() as (root, base):
            (root/'a/src/lib.rs').write_text('pub fn changed() {}\n')
            commit = commit_paths(root, ['a/src/lib.rs'])
            result = verify_candidate(root, base, commit, mode='full', require_full=True,
                verify_base=False, omit=lambda command: command[:2] == ['cargo', 'xwin'])
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('explicit diff base', result.stderr)

    def test_rust_receipt_cannot_omit_either_windows_compile_or_strict_lint(self):
        with landing_repository() as (root, base):
            (root/'a/src/lib.rs').write_text('pub fn changed() {}\n')
            commit = commit_paths(root, ['a/src/lib.rs'])
            for mode in ('affected', 'full'):
                for operation in ('test', 'clippy'):
                    with self.subTest(mode=mode, operation=operation):
                        result = verify_candidate(root, base, commit, mode=mode,
                            omit=lambda command: command[:3] == ['cargo', 'xwin', operation])
                        self.assertNotEqual(result.returncode, 0)
                        self.assertIn('missing required command', result.stderr)

    def test_receipt_requires_each_ordinary_and_fixture_windows_check(self):
        with landing_repository() as (root, base):
            (root/'Cargo.toml').write_text('[workspace]\nmembers=["a","b","c","product"]\nresolver="2"\n')
            (root/'product/src').mkdir(parents=True)
            (root/'product/src/lib.rs').write_text('pub fn fixture() {}\n')
            (root/'product/Cargo.toml').write_text(
                '[package]\nname="grapt-product"\nversion="0.1.0"\nedition="2021"\n'
                '[features]\ncheckpoint-fixture=[]\nhandover-crash-fixture=[]\nloopback-provider-testkit=[]\n')
            subprocess.run(['cargo', 'generate-lockfile', '--offline'], cwd=root,
                           check=True, capture_output=True, env=fixture_environment())
            commit = commit_paths(root, ['Cargo.toml', 'Cargo.lock', 'product/Cargo.toml', 'product/src/lib.rs'])
            for mode in ('affected', 'full'):
                for operation in ('test', 'clippy'):
                    for fixture in (False, True):
                        with self.subTest(mode=mode, operation=operation, fixture=fixture):
                            result = verify_candidate(root, base, commit, mode=mode,
                                omit=lambda command: command[:3] == ['cargo', 'xwin', operation]
                                and ('--features' in command) == fixture)
                            self.assertNotEqual(result.returncode, 0)
                            self.assertIn('missing required command', result.stderr)

    def test_optional_command_elapsed_is_attached_to_success_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/"gate.log"
            log.write_text("# secode-gate-run 0123456789abcdef0123456789abcdef\nsuccess\n", newline="\n")
            source = {"commit":"fixture", "clean":True}
            commands = [["first"], ["second"]]
            with patch.object(receipt, "source", return_value=source):
                receipt.write(log, "affected", source, commands, 4, command_seconds=[1.25, 2.5], nextest_junit=nextest_result(log,commands))
            record = json.loads(log.with_suffix(".log.receipt.json").read_text())
            self.assertEqual(record["commands"], commands)
            self.assertEqual(record["command_seconds"], [1.25, 2.5])
            self.assertEqual(record["exit"], 0)
            self.assertEqual(record["seconds"], 4)

    def test_unchanged_build_helpers_are_not_required_for_a_rust_slice(self):
        with landing_repository() as (root, base):
            (root/'a/src/lib.rs').write_text('pub fn changed() {}\n')
            commit = commit_paths(root, ['a/src/lib.rs'])
            result = verify_candidate(root, base, commit,
                omit=lambda command: 'scripts/build' in command)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_build_control_receipt_cannot_omit_its_selected_helpers(self):
        with landing_repository() as (root, base):
            script = root/'scripts/build/changed.py'
            script.parent.mkdir(parents=True)
            script.write_text('# changed helper\n')
            commit = commit_paths(root, ['scripts/build/changed.py'])
            result = verify_candidate(root, base, commit, mode='full',
                omit=lambda command: 'scripts/build' in command)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('missing required command', result.stderr)

    def test_docs_only_affected_receipt_accepts_current_dev_base_through_cli(self):
        with landing_repository() as (root, base):
            path = root/'docs/report.md'
            path.parent.mkdir(); path.write_text('report\n')
            commit = commit_paths(root, ['docs/report.md'])
            result = verify_candidate(root, base, commit)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('exact-source affected PASS', result.stdout)

    def test_tier2_rust_accepts_affected_but_tier1_requires_full(self):
        with landing_repository() as (root, base):
            (root/'a/src/lib.rs').write_text('pub fn changed() {}\n')
            commit = commit_paths(root, ['a/src/lib.rs'])
            result = verify_candidate(root, base, commit)
            self.assertEqual(result.returncode, 0, result.stderr)
            tier1 = verify_candidate(root, base, commit, require_full=True)
            self.assertNotEqual(tier1.returncode, 0)
            self.assertIn('requires a full', tier1.stderr)
            full = verify_candidate(root, base, commit, mode='full', require_full=True)
            self.assertEqual(full.returncode, 0, full.stderr)

    def test_new_workspace_member_keeps_affected_coverage(self):
        with landing_repository() as (root, base):
            (root/'Cargo.toml').write_text('[workspace]\nmembers=["a", "b", "c", "d"]\nresolver="2"\n')
            (root/'d/src').mkdir(parents=True)
            (root/'d/Cargo.toml').write_text('[package]\nname="d"\nversion="0.1.0"\nedition="2021"\n')
            (root/'d/src/lib.rs').write_text('pub fn added() {}\n')
            subprocess.run(['cargo', 'generate-lockfile', '--offline'], cwd=root, env=fixture_environment(), check=True, capture_output=True)
            commit = commit_paths(root, ['Cargo.toml', 'Cargo.lock', 'd/Cargo.toml', 'd/src/lib.rs'])
            paths = {'Cargo.toml', 'Cargo.lock', 'd/Cargo.toml', 'd/src/lib.rs'}
            scoped = gate.gate_plan.scoped_paths(paths, root, base)
            self.assertEqual(scoped, {'d/Cargo.toml', 'd/src/lib.rs'})
            metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'], cwd=root, env=fixture_environment()))
            self.assertEqual(gate.select(metadata['packages'], scoped, root), ['d'])
            accepted = verify_candidate(root, base, commit)
            self.assertEqual(accepted.returncode, 0, accepted.stderr)
            tier1 = verify_candidate(root, base, commit, require_full=True)
            self.assertNotEqual(tier1.returncode, 0)
            self.assertIn('requires a full', tier1.stderr)
            # A workspace lint change is still shared build policy.
            with (root/'Cargo.toml').open('a') as output:
                output.write('\n[workspace.lints.rust]\nunused_must_use="deny"\n')
            self.assertEqual(gate.gate_plan.scoped_paths(paths, root, base), paths)

    def test_extraction_with_local_consumer_edges_keeps_only_affected_crates(self):
        with landing_repository() as (root, base):
            (root/'Cargo.toml').write_text('[workspace]\nmembers=["a", "b", "c", "d"]\nresolver="2"\n')
            (root/'d/src').mkdir(parents=True)
            (root/'d/Cargo.toml').write_text('[package]\nname="d"\nversion="0.1.0"\nedition="2021"\n')
            (root/'d/src/lib.rs').write_text('pub fn added() {}\n')
            with (root/'a/Cargo.toml').open('a') as output:
                output.write('d={path="../d"}\n')
            subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,env=fixture_environment(),check=True,capture_output=True)
            paths = {'Cargo.toml','Cargo.lock','d/Cargo.toml','d/src/lib.rs','a/Cargo.toml'}
            commit = commit_paths(root,sorted(paths))
            scoped = gate.gate_plan.scoped_paths(paths,root,base)
            self.assertEqual(scoped,paths-{'Cargo.toml','Cargo.lock'})
            metadata=json.loads(subprocess.check_output(['cargo','metadata','--locked','--no-deps','--format-version','1'],cwd=root,env=fixture_environment()))
            self.assertEqual(gate.select(metadata['packages'],scoped,root),['a','b','d'])
            accepted=verify_candidate(root,base,commit)
            self.assertEqual(accepted.returncode,0,accepted.stderr)
            # A changed lock edge without its consumer manifest is not scoped.
            self.assertIn('Cargo.lock',gate.gate_plan.scoped_paths(paths-{'a/Cargo.toml'},root,base))
            # Other existing package fields/resolutions remain broad changes.
            raw=(root/'Cargo.lock').read_text()
            (root/'Cargo.lock').write_text(raw.replace('name = "c"\nversion = "0.1.0"','name = "c"\nversion = "0.2.0"'))
            self.assertIn('Cargo.lock',gate.gate_plan.scoped_paths(paths,root,base))

    def test_removed_workspace_member_is_not_a_member_only_addition(self):
        with landing_repository() as (root, base):
            (root/'Cargo.toml').write_text('[workspace]\nmembers=["a", "b"]\nresolver="2"\n')
            paths={'Cargo.toml','Cargo.lock'}
            self.assertEqual(gate.gate_plan.scoped_paths(paths,root,base),paths)

    def test_existing_member_new_dependency_keeps_touched_crates_and_direct_consumers(self):
        with tempfile.TemporaryDirectory() as external, landing_repository() as (root, base):
            dependency = Path(external)
            (dependency/'src').mkdir()
            (dependency/'src/lib.rs').write_text('pub fn image() {}\n')
            (dependency/'Cargo.toml').write_text('[package]\nname="image-fixture"\nversion="0.1.0"\nedition="2021"\n')
            with (root/'a/Cargo.toml').open('a') as output:
                output.write(f'image-fixture={{path="{dependency}"}}\n')
            subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,env=fixture_environment(),check=True,capture_output=True)
            paths = {'Cargo.lock', 'a/Cargo.toml'}
            scoped = gate.gate_plan.scoped_paths(paths,root,base)
            self.assertEqual(scoped, {'a/Cargo.toml'})
            metadata = json.loads(subprocess.check_output(['cargo','metadata','--locked','--no-deps','--format-version','1'],cwd=root,env=fixture_environment()))
            self.assertEqual(gate.select(metadata['packages'],scoped,root), ['a','b'])
            commit=commit_paths(root,list(paths))
            result=verify_candidate(root,base,commit)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(gate.gate_plan.scoped_paths({'Cargo.lock'},root,base),{'Cargo.lock'})
            older=commit
            (dependency/'Cargo.toml').write_text((dependency/'Cargo.toml').read_text().replace('0.1.0','0.2.0'))
            subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,env=fixture_environment(),check=True,capture_output=True)
            self.assertEqual(gate.gate_plan.scoped_paths(paths,root,older),paths)

    def test_docs_and_ordinary_manifests_do_not_require_full(self):
        for path in ['crates/grapt-store/README.md', 'docs/credentials.md',
                     'crates/grapt-store/docs/overview.json', 'crates/grapt-api/Cargo.toml',
                     'Cargo.toml', 'a/Cargo.toml']:
            with self.subTest(path=path), landing_repository() as (root, base):
                target = root/path
                target.parent.mkdir(parents=True, exist_ok=True)
                if target.exists():
                    target.write_text(target.read_text()+'\n# ordinary manifest comment\n')
                else:
                    target.write_text('# documentation fixture\n')
                commit = commit_paths(root, [path])
                result = verify_candidate(root, base, commit)
                self.assertEqual(result.returncode, 0, result.stderr)

    def test_neutral_lockfile_comments_stay_scoped_but_toolchain_changes_require_full(self):
        for path in ['Cargo.lock', 'rust-toolchain.toml']:
            with self.subTest(path=path), landing_repository() as (root, base):
                target = root/path
                target.write_text(target.read_text()+'\n' if target.exists()
                                  else '[toolchain]\nchannel="1.97.1"\n')
                commit = commit_paths(root, [path])
                result = verify_candidate(root, base, commit)
                if path == 'Cargo.lock':
                    self.assertEqual(result.returncode, 0, result.stderr)
                else:
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('Changed paths require a full', result.stderr)
                    self.assertEqual(verify_candidate(root, base, commit, mode='full').returncode, 0)

    def test_affected_refuses_missing_checks_lint_test_fmt_and_laws(self):
        omitted = [lambda c: len(c)>1 and c[1]=='clippy',
                   lambda c: len(c)>1 and c[1]=='fmt',
                   lambda c: len(c)>1 and c[1]=='test' and 'a' in c,
                   lambda c: 'no_god_files' in c, lambda c: 'write_chokepoint' in c]
        for omit in omitted:
            with self.subTest(omit=omit), landing_repository() as (root, base):
                (root/'a/src/lib.rs').write_text('pub fn changed() {}\n')
                commit = commit_paths(root, ['a/src/lib.rs'])
                result = verify_candidate(root, base, commit, omit=omit)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('missing required command', result.stderr)

    def test_affected_receipt_refuses_stale_base_before_the_diff(self):
        with landing_repository() as (root, base):
            (root/'README.md').write_text('new docs\n')
            commit = commit_paths(root, ['README.md'])
            result = verify_candidate(root, base, commit, recorded_base='f'*40)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('base differs from current main', result.stderr)

    def test_bench_affected_receipt_requires_its_benchmark_checks(self):
        with landing_repository() as (root, base):
            path = root/'bench/test_runner.py'; path.parent.mkdir(); path.write_text('# changed\n')
            commit = commit_paths(root, ['bench/test_runner.py'])
            missing = verify_candidate(root, base, commit)
            self.assertNotEqual(missing.returncode, 0)
            self.assertIn('missing required command', missing.stderr)
            complete = verify_candidate(root, base, commit, bench_checks=True)
            self.assertEqual(complete.returncode, 0, complete.stderr)

    def test_slow_status_reports_dirty_source_without_refreshing_the_index(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            (root/'tracked').write_text('base')
            subprocess.run(['git', 'add', '--', 'tracked'], cwd=root, check=True)
            subprocess.run(['git', '-c', 'user.name=Gate', '-c', 'user.email=gate@localhost',
                            'commit', '-q', '-F', '-', '--', 'tracked'], cwd=root,
                           input=b'Owned receipt test baseline\n', check=True)
            commit = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root).decode().strip()
            (root/'tracked').write_text('changed')
            index = (root/'.git/index').read_bytes()
            real_output = subprocess.check_output
            def git_output(argv, **kwargs):
                return real_output(argv, cwd=root, **kwargs)
            # Check the slow-disk budget at its subprocess boundary while real
            # Git still proves dirty-byte detection and read-only index access.
            with patch.object(receipt.subprocess, 'check_output', side_effect=git_output) as process:
                self.assertEqual(receipt.source(), {'commit': commit, 'clean': False})
            process.assert_any_call(
                ['git', '--no-optional-locks', 'status', '--porcelain'], timeout=60)
            self.assertEqual((root/'.git/index').read_bytes(), index)
            self.assertFalse((root/'.git/index.lock').exists())

    def test_fifo_receipt_refuses_before_any_blocking_read(self):
        import os
        import subprocess
        import sys
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'gate.log'; log.write_text('regular log')
            os.mkfifo(log.with_suffix('.log.receipt.json'))
            result = subprocess.run([sys.executable, str(Path(receipt.__file__)), str(log), 'a'*40],
                                    capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b'bounded regular file', result.stderr)

    def test_only_fresh_unchanged_full_log_and_clean_exact_source_can_skip(self):
        with landing_repository() as (root, base), chdir(root):
            log = root / 'gate.log'
            log.write_text('# secode-gate-run 0123456789abcdef0123456789abcdef\nall commands exited zero\n', newline="\n")
            source = {'commit': base, 'clean': True}
            commands = [[sys.executable, 'scripts/release/licenses.py', 'check'],
                        [sys.executable, 'scripts/build/ratchet_monotonic.py', base],
                        *gate.full_commands(['grapt-product'])]
            with patch.object(receipt, 'source', return_value=source), patch.object(receipt.time, 'time', return_value=1000):
                receipt.write(log, 'full', source, commands, 5, base=base, nextest_junit=nextest_result(log,commands))
            original = log.with_suffix('.log.receipt.json').read_text()
            receipt.verify(log, source['commit'], now=1001, base=base)
            parallel = json.loads(original)
            workspace_test(parallel['commands'])[-1] = '--test-threads=4'
            log.with_suffix('.log.receipt.json').write_text(json.dumps(parallel))
            with self.assertRaisesRegex(ValueError, 'missing required workspace commands'):
                receipt.verify(log, source['commit'], now=1001, base=base)
            missing_feature = json.loads(original)
            command = workspace_test(missing_feature['commands'])
            feature = command.index('--features')
            del command[feature:feature+2]
            log.with_suffix('.log.receipt.json').write_text(json.dumps(missing_feature))
            with self.assertRaises(ValueError):
                receipt.verify(log, source['commit'], now=1001, base=base)
            for omitted in ['grapt-cli/loopback-testkit',
                            'grapt-mcp-client/loopback-testkit',
                            'grapt-product/checkpoint-fixture',
                            'grapt-product/handover-crash-fixture']:
                missing = json.loads(original)
                command = workspace_test(missing['commands'])
                position = command.index('--features') + 1
                command[position] = ','.join(value for value in command[position].split(',')
                                             if value != omitted)
                log.with_suffix('.log.receipt.json').write_text(json.dumps(missing))
                with self.subTest(omitted=omitted), self.assertRaisesRegex(
                        ValueError, 'missing required workspace commands'):
                    receipt.verify(log, source['commit'], now=1001, base=base)
            missing_default_check = json.loads(original)
            missing_default_check['commands'] = [row for row in missing_default_check['commands']
                if row != ['cargo', 'check', '--locked', '--workspace', '--tests']]
            log.with_suffix('.log.receipt.json').write_text(json.dumps(missing_default_check))
            with self.assertRaisesRegex(ValueError, 'missing required workspace commands'):
                receipt.verify(log, source['commit'], now=1001, base=base)
            missing_ordinary_handover = json.loads(original)
            missing_ordinary_handover['commands'] = [row for row in missing_ordinary_handover['commands']
                if row != gate.test_features.ordinary_commands()[0]]
            log.with_suffix('.log.receipt.json').write_text(json.dumps(missing_ordinary_handover))
            with self.assertRaisesRegex(ValueError, 'missing required workspace commands'):
                receipt.verify(log, source['commit'], now=1001, base=base)
            for field, value in [('mode', 'affected'), ('exit', 1), ('commands', []),
                                 ('nextest_junit', None), ('nextest_junit', '0'*64),
                                 ('source_before', {'commit': base, 'clean': False}),
                                 ('source_after', {'commit': 'b' * 40, 'clean': True})]:
                changed = json.loads(original)
                changed[field] = value
                log.with_suffix('.log.receipt.json').write_text(json.dumps(changed))
                with self.subTest(field=field), self.assertRaises(ValueError):
                    receipt.verify(log, source['commit'], now=1001, base=base)
            log.with_suffix('.log.receipt.json').write_text(original)
            for now in [999, 100000]:
                with self.assertRaises(ValueError): receipt.verify(log, source['commit'], now=now, base=base)
            log.write_text('changed evidence')
            with self.assertRaises(ValueError): receipt.verify(log, source['commit'], now=1001, base=base)

    def test_full_receipt_recorded_before_newer_tags_keeps_its_stronger_coverage(self):
        with landing_repository() as (root, base), chdir(root):
            log = root / 'gate.log'
            log.write_text('# secode-gate-run 0123456789abcdef0123456789abcdef\nall commands exited zero\n', newline="\n")
            source = {'commit': base, 'clean': True}
            commands = [[sys.executable, 'scripts/release/licenses.py', 'check'],
                        [sys.executable, 'scripts/build/ratchet_monotonic.py', base],
                        *gate.full_commands([])]
            with patch.object(receipt, 'source', return_value=source), patch.object(receipt.time, 'time', return_value=1000):
                receipt.write(log, 'full', source, commands, 5, base=base, nextest_junit=nextest_result(log,commands))
            original = log.with_suffix('.log.receipt.json').read_text()
            for runner in ('cargo', 'nextest'):
                bare = json.loads(original)
                command = (workspace_test(bare['commands']) if runner == 'cargo' else
                           next(row for row in bare['commands'] if row[:3] == ['cargo','nextest','run']))
                if runner == 'cargo':
                    while '--skip' in command:
                        position = command.index('--skip')
                        del command[position:position+2]
                    command.remove('--exact')
                else:
                    position = command.index('-E')
                    del command[position:position+2]
                log.with_suffix('.log.receipt.json').write_text(json.dumps(bare))
                receipt.verify(log, source['commit'], now=1001, base=base)
                # A narrower selection cannot borrow the green full result.
                command.extend(['--exact','--skip','not_tagged'] if runner == 'cargo' else
                               ['-E','not (test(=not_tagged))'])
                log.with_suffix('.log.receipt.json').write_text(json.dumps(bare))
                with self.subTest(runner=runner), self.assertRaisesRegex(ValueError, 'missing required workspace commands'):
                    receipt.verify(log, source['commit'], now=1001, base=base)

    def test_affected_receipt_recorded_before_newer_tags_keeps_its_stronger_coverage(self):
        with landing_repository() as (root, base):
            (root/'a/src/lib.rs').write_text('pub fn value() {}\npub fn tagged() {}\n')
            candidate = commit_paths(root, ['a/src/lib.rs'])
            paths = {p for p in subprocess.check_output(
                ['git', 'diff', '--no-renames', '--name-only', base, candidate], cwd=root, text=True).splitlines() if p}
            metadata = json.loads(subprocess.check_output(
                ['cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'],
                cwd=root, env=fixture_environment()))
            old_tags = {'a': {'tagged_test': 'a/src/lib.rs'}}
            new_tags = {'a': {'tagged_test': 'a/src/lib.rs', 'tagged_second': 'a/src/lib.rs'}}
            with patch.dict(receipt.slow_tests.TESTS, old_tags, clear=False):
                commands = gate.gate_plan.commands('affected', metadata['packages'], paths, root, base=base)
            log = root/'gate.log'
            log.write_text('# secode-gate-run 0123456789abcdef0123456789abcdef\nall selected commands exited zero\n', newline="\n")
            source = {'commit': candidate, 'clean': True}
            old_cwd = os.getcwd()
            try:
                os.chdir(root)
                with patch.object(receipt, 'source', return_value=source), patch.object(receipt.time, 'time', return_value=1000), \
                        patch.dict(receipt.slow_tests.TESTS, new_tags, clear=False):
                    receipt.write(log, 'affected', source, commands, 1, base=base, nextest_junit=nextest_result(log,commands))
                    receipt.verify(log, candidate, now=1001, base=base)
                    bare = json.loads(log.with_suffix('.log.receipt.json').read_text())
                    recorded = next(command for command in bare['commands']
                                    if command[0] == 'cargo' and command[1] == 'test')
                    while '--skip' in recorded:
                        position = recorded.index('--skip')
                        del recorded[position:position + 2]
                    if '--exact' in recorded:
                        recorded.remove('--exact')
                    log.with_suffix('.log.receipt.json').write_text(json.dumps(bare))
                    receipt.verify(log, candidate, now=1001, base=base)
                    record = json.loads(log.with_suffix('.log.receipt.json').read_text())
                    recorded = next(command for command in record['commands']
                                    if command[0] == 'cargo' and command[1] == 'test')
                    recorded.append('--skip')
                    recorded.append('tagged_extra')
                    log.with_suffix('.log.receipt.json').write_text(json.dumps(record))
                    with self.assertRaisesRegex(ValueError, 'missing required command'):
                        receipt.verify(log, candidate, now=1001, base=base)
            finally:
                os.chdir(old_cwd)
