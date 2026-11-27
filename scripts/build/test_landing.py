"""Real Git rebases and tiny offline Rust checks at the landing boundary."""
import base64
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import gate_plan
import landing
from fixture_environment import nextest_result, isolated_environment

HELPER = Path(landing.__file__).resolve()


def command(root, *args, env=None):
    return subprocess.check_output(args, cwd=root, env=env, stderr=subprocess.STDOUT,
                                   timeout=60).decode().strip()


def commit(root, paths):
    command(root, 'git', 'add', '--', *paths)
    subprocess.run(['git', '-c', 'user.name=Fixture Author',
                    '-c', 'user.email=fixture@users.noreply.github.com',
                    'commit', '-q', '-F', '-', '--', *paths], cwd=root,
                   input=b'chore(fixture): own the landing fixture\n\n'
                         b'Co-Authored-By: Codex <noreply@openai.com>\n',
                   check=True, timeout=30)
    return command(root, 'git', 'rev-parse', 'HEAD')


@contextmanager
def fixture(a_source="pub fn value() -> u32 { 1 }\n", a_lints="", legacy_gate=False):
    with tempfile.TemporaryDirectory(prefix='secode-land-test-') as directory:
        outer = Path(directory)
        canonical = outer/'canonical'; canonical.mkdir()
        command(canonical, 'git', 'init', '-q', '-b', 'main')
        (canonical/'.gitignore').write_text('__pycache__/\n')
        (canonical/'Cargo.toml').write_text('[workspace]\nmembers=["a","b"]\nresolver="2"\n')
        (canonical/'README.md').write_text('base\n')
        build = canonical/'scripts/build'; build.mkdir(parents=True)
        # Use the real verifier, planner and bounded process runner. The fixture
        # receipt below is test input, not an assertion that a full gate ran.
        for path in HELPER.parent.glob('*.py'):
            if not path.name.startswith('test_') or path.name == 'test_features.py':
                shutil.copyfile(path, build/path.name)
        # Platform compilation has separate carrier witnesses. These disposable
        # Git fixtures keep their two tiny compiler targets local and offline.
        (build/'windows_gate.py').write_text(
            'def commands(packages, *, workspace=False): return []\n'
            'def environment(command): return None\n')
        if legacy_gate:
            path = build/'gate.py'
            text = path.read_text()
            anchor = 'descriptors = set(heavy.inherited_fds())'
            if text.count(anchor) != 1:
                raise AssertionError('Legacy gate descriptor boundary changed')
            path.write_text(text.replace(anchor, 'descriptors = set()'))
        (canonical/'scripts/shared-cargo.sh').write_text('exec cargo "$@"\n')
        # The tiny compiler fixture runs the actual gate policy/process runner;
        # cache ownership is isolated here. RAM supervision is witnessed with
        # the ordinary gate separately, rather than allocating 24 GiB per unit.
        # Each admission supplies a fresh lease, including per-Cargo admission;
        # the real runner still inherits and closes that descriptor.
        (canonical/'scripts/gate.sh').write_text('''if [ "$1" != composition ]; then exit 0; fi
exec python3 -c 'import signal, sys, tempfile; from pathlib import Path; signal.signal(signal.SIGTERM, signal.default_int_handler); sys.path.insert(0,"scripts/build"); import full_capacity; full_capacity.LOCK=Path.cwd().parent/"full.lock"; import gate; import heavy_sampling; from fixture_environment import hardware_sample; heavy_sampling.host=hardware_sample; gate.evict_targets.prepare=lambda *a, **k: tempfile.TemporaryFile(); gate.evict_targets.inherit=lambda *a, **k: None; gate.evict_targets.retain=lambda *a: None; gate.claim_target=lambda *a: None; gate.reserve_composition_log(gate.parse_args()); sys.exit(gate.main())' "$@"
''')
        for name in ['a', 'b']:
            (canonical/name/'src').mkdir(parents=True)
            (canonical/name/'Cargo.toml').write_text(
                f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2021"\n')
            (canonical/name/'src/lib.rs').write_text(a_source if name == 'a' else 'pub fn value() -> u32 { 1 }\n')
            if name == 'a' and a_lints:
                with (canonical/name/'Cargo.toml').open('a') as output:
                    output.write(a_lints)
        env = isolated_environment()
        (outer/'target').mkdir()
        env['CARGO_TARGET_DIR'] = str(outer/'target')
        env['CARGO_HOME'] = str(outer/'cargo-home')
        env['RUSTUP_TOOLCHAIN'] = '1.97.1'
        env['SECODE_TARGET_ROOT'] = str(outer/'cache')
        env['SECODE_FIXTURES'] = str(outer/'fixtures')
        env['SECODE_WORKTREE_PARENT'] = str(outer)
        (outer/'fixtures').mkdir()
        command(canonical, 'cargo', 'generate-lockfile', '--offline', env=env)
        tracked = [p.relative_to(canonical).as_posix() for p in canonical.rglob('*')
                   if p.is_file() and '.git' not in p.relative_to(canonical).parts]
        base = commit(canonical, tracked)
        root = outer/'candidate'
        command(outer, 'git', 'clone', '-q', str(canonical), str(root))
        command(root, 'git', 'checkout', '-q', '-b', 'candidate')
        command(root, 'git', 'config', 'user.name', 'Fixture Author')
        command(root, 'git', 'config', 'user.email', 'fixture@users.noreply.github.com')
        with patch.object(gate_plan.windows_gate, 'commands', return_value=[]), \
                patch.dict(os.environ, {'SECODE_WORKTREE_PARENT': str(outer)}):
            yield outer, canonical, root, base, env


def receipt_input(outer, root, base, accepted, full=False):
    paths = landing.paths(root, base, accepted)
    metadata = json.loads(command(root, 'cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1'))
    mode = 'full' if full else 'affected'
    log = outer/'original.log'; log.write_text('Fixture original gate input\n')
    source = {'commit': accepted, 'clean': True}
    record = {'schema': 'secode.gate.v1', 'mode': mode, 'exit': 0, 'base': base,
              'source_before': source, 'source_after': source, 'finished_unix': time.time(),
              'commands': gate_plan.commands(mode, metadata['packages'], paths, root, base=base),
              'log_sha256': landing.digest(log)}
    checksum=nextest_result(log,record['commands'])
    if checksum is not None:record['nextest_junit']=checksum
    log.with_suffix('.log.receipt.json').write_text(json.dumps(record))
    return {'accepted': accepted, 'old_base': base, 'branch': 'candidate',
            'canonical': str(outer/'canonical'), 'gate_log': str(log),
            'full': full, 'run_gate': False, 'receipt': str(outer/'composition.json')}


@contextmanager
def exported_checkout(root, request):
    branch = command(root, 'git', 'branch', '--show-current')
    command(root, 'git', 'checkout', '-q', landing.export_branch(request))
    try:
        yield root
    finally:
        command(root, 'git', 'checkout', '-q', branch)


def helper_command(request, mode='compose'):
    encoded = base64.b64encode(json.dumps(request).encode()).decode()
    # Keep nested landing tests independent of the host-wide gate lease.
    bootstrap = ("import sys, runpy; from pathlib import Path; "
                 "sys.path.insert(0,str(Path(sys.argv[1]).parent)); import heavy, ram_targets, heavy_sampling, full_capacity; "
                 "from fixture_environment import hardware_sample; heavy_sampling.host=hardware_sample; "
                 "ram_targets.allocate=lambda *a: (_ for _ in ()).throw(ValueError('tiny fixture stays exclusive')); "
                 "heavy.HEAVY_LOCK=Path.cwd().parent/'heavy.lock'; "
                 "heavy.QUEUE_PATH=Path.cwd().parent/'heavy-queue'; "
                 "full_capacity.LOCK=Path.cwd().parent/'full.lock'; "
                 "sys.argv=sys.argv[1:]; runpy.run_path(sys.argv[0],run_name='__main__')")
    return [sys.executable, '-c', bootstrap, str(HELPER), mode, encoded]


def invoke(root, request, env, mode='compose', timeout=90):
    # Every real-Git success and failure must preserve the owner's checkout,
    # index, ref and unrelated registered worktrees.
    def owner_state():
        return tuple(command(root, 'git', *args) for args in [
            ('rev-parse', 'HEAD'), ('rev-parse', 'refs/heads/' + request['branch']),
            ('branch', '--show-current'), ('status', '--porcelain'),
            ('worktree', 'list', '--porcelain')])
    before = owner_state()
    argv = helper_command(request, mode)
    process = subprocess.Popen(argv, cwd=root, env=env, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, text=True)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        # Let the real helper join its gate child before killing the probe.
        # subprocess.run's timeout kills only the helper, orphaning that child.
        process.send_signal(signal.SIGTERM)
        try:
            process.communicate(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate(timeout=5)
        raise
    if owner_state() != before:
        raise AssertionError("Landing changed the owning checkout, branch or registered worktrees")
    if process.returncode:
        # Preserve the actual nested failure before TemporaryDirectory removes it.
        for suffix in ('.driver.log', '.log'):
            path = Path(request['receipt']).with_suffix(suffix)
            if path.is_file():
                with path.open('rb') as log:
                    log.seek(max(0, path.stat().st_size - 16384))
                    stderr += "\n" + log.read(16384).decode('utf-8', errors='replace')
    return subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)


class LandingTests(unittest.TestCase):
    def test_lease_holding_qualification_and_composition_share_one_reservation(self):
        import gate
        lanes = []
        held = []
        @contextmanager
        def slot(lane, **kwargs):
            expected = {'job_owner': 'agt-demo', 'ram': False,
                        'nvme': Path('/srv/secode/cache/target')}
            for key, value in expected.items():
                self.assertEqual(kwargs.get(key), value)
            self.assertEqual(held, [], 'nested heavy lock acquisition')
            held.append(lane)
            lanes.append(lane)
            try:
                with tempfile.TemporaryFile() as lease:
                    yield lease
            finally:
                held.clear()
        def execute(*args, **kwargs):
            self.assertEqual(held, ['landing'])
            self.assertIn('full', args[0])
            self.assertGreaterEqual(kwargs['lease_fd'], 0)
        def compose_locked(*args, **kwargs):
            self.assertEqual(held, ['landing'])
            self.assertGreaterEqual(kwargs['lease_fd'], 0)
            return 'composed'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'scripts/build').mkdir(parents=True)
            (root/'scripts/build/ram_targets.py').write_text('HEAVY_RAM_PROTOCOL = 1\n')
            request = {'accepted': 'a'*40, 'old_base': 'b'*40, 'branch': 'candidate',
                       'run_gate': True, 'receipt': str(root/'composition.json'),
                       'gate_log': str(root/'gate.log'), 'full': True, 'owner': 'agt-demo'}
            with patch.object(landing, 'source'), patch.object(landing.heavy, 'slot', slot), \
                    patch.object(landing.full_capacity, 'LOCK', root/'full.lock'), \
                    patch.object(landing.heavy, 'nvme_target', return_value=Path('/srv/secode/cache/target')), \
                    patch.object(gate, 'execute', execute), \
                    patch.object(landing, 'compose_locked', compose_locked):
                self.assertEqual(landing.compose(root, request), 'composed')
        self.assertEqual(lanes, ['landing'])

    def prepare(self, outer, root, base, path='a/src/lib.rs', text='pub fn value() -> u32 { 2 }\n', full=False):
        (root/path).write_text(text)
        accepted = commit(root, [path])
        return receipt_input(outer, root, base, accepted, full=full)

    def test_failed_isolated_composition_preserves_unrelated_worktrees_and_existing_paths(self):
        with fixture() as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            accepted = request['accepted']
            other = outer / 'unrelated-worktree'
            command(root, 'git', 'worktree', 'add', '--detach', str(other), accepted)
            before = command(root, 'git', 'worktree', 'list', '--porcelain')
            with patch.object(landing, 'compose_detached', side_effect=RuntimeError('gate failure')):
                with self.assertRaisesRegex(RuntimeError, 'gate failure'):
                    landing.compose_locked(root, request, lease_fd=None)
            self.assertEqual(command(root, 'git', 'worktree', 'list', '--porcelain'), before)
            self.assertEqual(command(root, 'git', 'rev-parse', 'candidate'), accepted)
            self.assertEqual(command(root, 'git', 'status', '--porcelain'), '')
            name = 'secode-land-' + landing.hashlib.sha256(str(root).encode()).hexdigest()[:32]
            reserved = root.parent / name
            reserved.mkdir()
            (reserved / 'owned.txt').write_text('preserve me')
            with self.assertRaisesRegex(ValueError, 'already exists'):
                with landing.detached_source(root, accepted):
                    self.fail('adopted an existing private path')
            self.assertEqual((reserved / 'owned.txt').read_text(), 'preserve me')
            self.assertEqual(command(root, 'git', 'worktree', 'list', '--porcelain'), before)
            command(root, 'git', 'worktree', 'remove', str(other))

    def test_disjoint_advance_rebases_and_preserves_original_qualification(self):
        for full in [False, True]:
            with self.subTest(full=full), fixture() as (outer, canonical, root, base, env):
                request = self.prepare(outer, root, base, full=full)
                original = Path(request['gate_log']).with_suffix('.log.receipt.json').read_bytes()
                (canonical/'README.md').write_text('new canonical documentation\n')
                advanced = commit(canonical, ['README.md'])
                result = invoke(root, request, env)
                self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
                record = json.loads(Path(request['receipt']).read_text())
                self.assertEqual(record['old_base'], base)
                self.assertEqual(record['new_base'], advanced)
                self.assertNotEqual(record['candidate'], request['accepted'])
                self.assertEqual(record['commands'], gate_plan.composition_commands(['a']))
                self.assertFalse(Path(request['receipt']).with_suffix('.log.receipt.json').exists(),
                                 'quick checks must not publish an affected/full gate receipt')
                self.assertEqual(original, Path(request['gate_log']).with_suffix('.log.receipt.json').read_bytes())
                self.assertEqual(command(canonical, 'git', 'rev-parse', 'HEAD'), advanced)
                request.update(candidate=record['candidate'], new_base=advanced)
                verified = invoke(root, request, env, 'verify')
                self.assertEqual(verified.returncode, 0, verified.stdout+verified.stderr)

    def test_new_direct_consumer_expands_checks_without_reinterpreting_old_receipt(self):
        with fixture() as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            with (canonical/'b/Cargo.toml').open('a') as output:
                output.write('[dependencies]\na={path="../a"}\n')
            command(canonical, 'cargo', 'generate-lockfile', '--offline', env=env)
            advanced = commit(canonical, ['b/Cargo.toml', 'Cargo.lock'])
            result = invoke(root, request, env)
            self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
            record = json.loads(Path(request['receipt']).read_text())
            self.assertEqual(record['commands'], gate_plan.composition_commands(['a', 'b']))
            request.update(candidate=record['candidate'], new_base=advanced)
            verified = invoke(root, request, env, 'verify')
            self.assertEqual(verified.returncode, 0, verified.stdout+verified.stderr)

    def test_live_gate_reloads_the_advanced_build_policy(self):
        with fixture() as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            request['run_gate'] = True
            policy = canonical/'scripts/build/gate_plan.py'
            text = policy.read_text()
            anchor = '    return result\n\n\ndef law_commands():'
            self.assertEqual(text.count(anchor), 1)
            policy.write_text(text.replace(anchor, '    result.append(["cargo", "check", "--locked", "-p", "b"])\n'+anchor))
            advanced = commit(canonical, ['scripts/build/gate_plan.py'])
            result = invoke(root, request, env)
            self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
            record = json.loads(Path(request['receipt']).read_text())
            self.assertEqual(record['commands'][-1], ['cargo', 'check', '--locked', '-p', 'b'])
            request.update(candidate=record['candidate'], new_base=advanced)
            verified = invoke(root, request, env, 'verify')
            self.assertEqual(verified.returncode, 0, verified.stdout+verified.stderr)

    def test_live_qualification_refreshes_legacy_runner_before_composition(self):
        with fixture(legacy_gate=True) as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            request['run_gate'] = True
            shutil.copyfile(HELPER.parent/'gate.py', canonical/'scripts/build/gate.py')
            advanced = commit(canonical, ['scripts/build/gate.py'])
            result = invoke(root, request, env)
            self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
            record = json.loads(Path(request['receipt']).read_text())
            self.assertEqual(record['new_base'], advanced)
            request.update(candidate=record['candidate'], new_base=advanced)
            verified = invoke(root, request, env, 'verify')
            self.assertEqual(verified.returncode, 0, verified.stdout+verified.stderr)

    def test_live_gate_reloads_the_advanced_windows_policy(self):
        with fixture() as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            request['run_gate'] = True
            policy = canonical/'scripts/build/windows_gate.py'
            policy.write_text(
                'def commands(packages, *, workspace=False):\n'
                '    return [["cargo", "check", "--locked", "-p", "b"]] if packages else []\n'
                'def environment(command): return None\n')
            advanced = commit(canonical, ['scripts/build/windows_gate.py'])
            result = invoke(root, request, env)
            self.assertEqual(result.returncode, 0, result.stdout+result.stderr)
            record = json.loads(Path(request['receipt']).read_text())
            self.assertEqual(record['commands'][-1], ['cargo', 'check', '--locked', '-p', 'b'])
            request.update(candidate=record['candidate'], new_base=advanced)
            verified = invoke(root, request, env, 'verify')
            self.assertEqual(verified.returncode, 0, verified.stdout+verified.stderr)

    def test_overlap_refuses_before_candidate_rebase(self):
        with fixture() as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            (canonical/'a/src/lib.rs').write_text('pub fn value() -> u32 { 3 }\n')
            commit(canonical, ['a/src/lib.rs'])
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('overlaps candidate path', result.stderr)
            self.assertEqual(command(root, 'git', 'rev-parse', 'HEAD'), request['accepted'])
            self.assertFalse(Path(request['receipt']).exists())

    def test_workspace_check_catches_disjoint_missing_field_in_other_tests(self):
        with fixture(a_source='pub struct Item { pub value: u32 }\n') as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base,
                                   text='pub struct Item { pub value: u32, pub extra: u32 }\n')
            (canonical/'b/src/lib.rs').write_text(
                '#[cfg(test)]\nmod source { include!("../../a/src/lib.rs"); }\n'
                '#[test] fn construct() { let _ = source::Item { value: 1 }; }\n')
            advanced = commit(canonical, ['b/src/lib.rs'])
            # Both original states compile. Only their disjoint composition fails.
            command(canonical, 'cargo', 'check', '--workspace', '--tests', env=env)
            command(root, 'cargo', 'check', '--workspace', '--tests', env=env)
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertFalse(Path(request['receipt']).exists(), 'workspace failure must not publish a receipt')
            self.assertIn('missing field', Path(request['receipt']).with_suffix('.log').read_text())
            self.assertEqual(command(canonical, 'git', 'rev-parse', 'HEAD'), advanced)

    def test_affected_clippy_catches_disjoint_lint_change(self):
        with fixture(a_lints='[lints.clippy]\nlen_without_is_empty="allow"\n') as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base, text='pub struct Widget;\nimpl Widget { pub fn len(&self) -> usize { 0 } }\n')
            manifest = canonical/'a/Cargo.toml'
            manifest.write_text(manifest.read_text().replace('="allow"', '="deny"'))
            commit(canonical, ['a/Cargo.toml'])
            command(canonical, 'cargo', 'clippy', '--workspace', '--all-targets', '--', '-D', 'warnings', env=env)
            command(root, 'cargo', 'clippy', '--workspace', '--all-targets', '--', '-D', 'warnings', env=env)
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertFalse(Path(request['receipt']).exists(), 'clippy failure must not publish a receipt')
            self.assertIn('len_without_is_empty', Path(request['receipt']).with_suffix('.log').read_text())

    def test_proof_tampering_and_wrong_tier_refuse(self):
        with fixture() as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            (canonical/'README.md').write_text('advanced\n'); advanced = commit(canonical, ['README.md'])
            result = invoke(root, request, env)
            self.assertEqual(result.returncode, 0, result.stderr)
            path = Path(request['receipt']); original = path.read_text(); record = json.loads(original)
            request.update(candidate=record['candidate'], new_base=advanced)
            for field, value in [('commands', []), ('old_base', 'f'*40), ('source_after', {}),
                                 ('finished_unix', 0), ('log_sha256', '0'*64)]:
                with self.subTest(field=field):
                    changed = json.loads(original); changed[field] = value; path.write_text(json.dumps(changed))
                    self.assertNotEqual(invoke(root, request, env, 'verify').returncode, 0)
            path.write_text(original); request['full'] = True
            self.assertNotEqual(invoke(root, request, env, 'verify').returncode, 0)
            request['full'] = False
            original_path = Path(request['gate_log']).with_suffix('.log.receipt.json')
            original_path.write_text(original_path.read_text()+' ')
            rejected = invoke(root, request, env, 'verify')
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn('Original qualification changed', rejected.stderr)

    def test_casefold_prefix_and_rename_paths_overlap(self):
        for owned, advanced in [({'File.rs'}, {'file.rs'}), ({'a/b'}, {'a'}), ({'a'}, {'a/b'})]:
            with self.subTest(owned=owned), self.assertRaisesRegex(ValueError, 'overlaps'):
                landing.assert_disjoint(owned, advanced)
        landing.assert_disjoint({'a/b'}, {'a/bc'})
        with fixture() as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            (canonical/'a/src/lib.rs').rename(canonical/'a/src/renamed.rs')
            commit(canonical, ['a/src/lib.rs', 'a/src/renamed.rs'])
            rejected = invoke(root, request, env)
            self.assertIn('overlaps candidate path', rejected.stderr)
            self.assertEqual(command(root, 'git', 'rev-parse', 'HEAD'), request['accepted'])

    def test_legacy_review_tier_cannot_waive_build_path_qualification(self):
        with fixture() as (outer, canonical, root, base, env):
            (root/'scripts/build/new_control.py').write_text('# build policy fixture\n')
            accepted=commit(root, ['scripts/build/new_control.py'])
            request=receipt_input(outer, root, base, accepted)
            self.assertFalse(request['full'])
            result=invoke(root, request, env, mode='qualify')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('requires a full', result.stderr)
            self.assertEqual(command(root, 'git', 'rev-parse', 'HEAD'), accepted)
            self.assertEqual(command(canonical, 'git', 'rev-parse', 'main'), base)

    def test_failed_original_tier_and_dirty_candidate_do_not_rebase(self):
        with fixture() as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            request['full'] = True
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('requires a full', result.stderr)
            self.assertEqual(command(root, 'git', 'rev-parse', 'HEAD'), request['accepted'])
            request['full'] = False; (root/'untracked.txt').write_text('owned work')
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('exact clean owning branch', result.stderr)

    def test_cancelled_live_gate_stops_its_process_group(self):
        with fixture() as (outer, canonical, root, base, env):
            # The child is owned by this fixture and never inspects host jobs.
            (root/'scripts/gate.sh').write_text('echo $$ > "$LANDING_PROBE_PID"\nexec sleep 30\n')
            accepted = commit(root, ['scripts/gate.sh'])
            request = receipt_input(outer, root, base, accepted)
            request['run_gate'] = True
            pid_path = outer/'child.pid'; env['LANDING_PROBE_PID'] = str(pid_path)
            process = subprocess.Popen(helper_command(request),
                                       cwd=root, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            pid = None
            try:
                for _ in range(100):
                    if pid_path.exists():
                        pid = int(pid_path.read_text()); break
                    if process.poll() is not None: break
                    time.sleep(0.05)
                self.assertIsNotNone(pid, 'live gate never started')
                process.send_signal(signal.SIGTERM)
                stdout, stderr = process.communicate(timeout=12)
                self.assertEqual(process.returncode, 130, stdout+stderr)
                with self.assertRaises(ProcessLookupError): os.kill(pid, 0)
                self.assertFalse(Path(request['receipt']).exists())
            finally:
                if process.poll() is None: process.kill(); process.wait(timeout=5)
                if pid is not None:
                    try: os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError: pass

    def test_replaced_landing_waiter_redeems_its_queue_credit_once(self):
        from heavy_queue import Queue
        with fixture() as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            queue = Queue(outer / 'heavy-queue', outer / 'heavy.lock')
            ticket, notification = queue.enqueue(0, request['branch'], time.monotonic() + 10)
            try:
                queue.refund(ticket)
                queue.remove(ticket)
            finally:
                notification.close()
            request['refund'] = ticket['id']
            result = invoke(root, request, env)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Refund is absent', result.stderr)

    def test_closed_request_and_file_bounds(self):
        with self.assertRaisesRegex(ValueError, '4 KiB'):
            landing.parse_request('A'*6000, 'compose')
        with self.assertRaisesRegex(ValueError, 'fields'):
            landing.parse_request(base64.b64encode(b'{}').decode(), 'compose')
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)/'pipe'; os.mkfifo(path)
            with self.assertRaisesRegex(ValueError, 'bounded regular'):
                landing.digest(path)

    def test_composition_uses_reviewed_allowance_inside_governed_ram_gate(self):
        from unittest.mock import patch
        import gate
        with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryFile() as lease:
            root = Path(directory)
            log = root/'composition.log'
            checks = [['cargo', 'check', '--locked', '--workspace', '--tests']]
            with patch.object(landing.importlib, 'reload', return_value=gate), \
                    patch.object(landing.time, 'monotonic', return_value=1000), \
                    patch.object(gate, 'execute') as execute:
                landing.run_checks(root, checks, log, 'a'*40, lease_fd=lease.fileno())
            command = execute.call_args.args[0]
            self.assertEqual(int(command[command.index('--timeout')+1]), gate.REVIEWED_GATE_TIMEOUT_SECONDS)
            self.assertGreaterEqual(execute.call_args.args[2],
                                    1000+gate.REVIEWED_GATE_TIMEOUT_SECONDS)
            self.assertEqual(execute.call_args.kwargs, {'cwd': root, 'lease_fd': lease.fileno()})
            self.assertTrue(log.with_suffix('.driver.log').is_file())

    def test_preexisting_composition_log_is_preserved_without_a_proof(self):
        with fixture() as (outer, canonical, root, base, env):
            request = self.prepare(outer, root, base)
            (canonical/'README.md').write_text('advance\n')
            commit(canonical, ['README.md'])
            log = Path(request['receipt']).with_suffix('.log')
            log.write_text('Preserve existing evidence\n')
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0, result.stdout)
            self.assertEqual(log.read_text(), 'Preserve existing evidence\n')
            self.assertFalse(Path(request['receipt']).exists())


if __name__ == '__main__':
    unittest.main()
