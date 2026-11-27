"""Shell, Cargo and admission checks across independent checkouts."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import shutil
import tempfile
import tomllib
import unittest
import evict_targets
import build_environment
import heavy
import install_schedules
from fixture_environment import isolated_environment


SCRIPTS = Path(__file__).resolve().parents[1]


class CacheTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.environment = isolated_environment()
        self.environment["SECODE_CACHE_DIR"] = str(self.directory / "cache")

        fake = self.directory / 'fake-tools'
        fake.mkdir()
        for name, body in [('rustc', 'printf "rustc fixture-version\\n"'), ('sccache', 'exit 99')]:
            path = fake/name
            path.write_text('#!/usr/bin/env bash\n'+body+'\n')
            path.chmod(0o755)
        self.environment['PATH']=str(fake)+os.pathsep+self.environment['PATH']
        # These subprocesses exercise fake compilers and tiny fixture caches.
        # Their capacity must not depend on the host's TMPDIR filesystem.
        (fake / 'sitecustomize.py').write_text(
            'from collections import namedtuple\n'
            'from pathlib import Path\n'
            'import shutil\n'
            '_real_disk_usage = shutil.disk_usage\n'
            f'_fixture_cache = Path({str(self.directory / "cache")!r}).resolve()\n'
            '_usage = namedtuple("usage", "total used free")\n'
            'def _fixture_disk_usage(path):\n'
            '    if Path(path).resolve() == _fixture_cache:\n'
            '        return _usage(200 * 1024**3, 100 * 1024**3, 100 * 1024**3)\n'
            '    return _real_disk_usage(path)\n'
            'shutil.disk_usage = _fixture_disk_usage\n'
        )
        self.environment['PYTHONPATH'] = os.pathsep.join(
            filter(None, [str(fake), self.environment.get('PYTHONPATH')])
        )

    def checkout(self, name):
        root = self.directory / name
        root.mkdir()
        subprocess.run(["git", "init", "-q", str(root)], check=True, timeout=10)
        return root

    def defaults(self, root):
        command = 'source "$1"; printf "%s\\n%s\\n" "$CARGO_TARGET_DIR" "$SECODE_TARGET_ROOT"'
        result = subprocess.run(["bash", "-eu", "-c", command, "cache-test", str(SCRIPTS / "build-cache.sh")],
                                cwd=root, env=self.environment, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True, timeout=20)
        return result.stdout.splitlines()

    def claim(self, root):
        """What a cache-layout shell gives a configured worktree."""
        command = 'source "$1" >/dev/null 2>&1; printf "%s\\n%s\\n%s\\n" "$SECODE_TARGET_ROOT" "$SECODE_WORKTREE_TARGET" "$SECODE_WORKTREE_ROOT"'
        result = subprocess.run(['bash','-eu','-c',command,'claim-test',str(SCRIPTS/'build-cache.sh')],
                                cwd=root, env=self.environment, capture_output=True, text=True,
                                check=True, timeout=20)
        target_root, worktree, checkout = result.stdout.splitlines()
        return dict(SECODE_TARGET_ROOT=target_root, SECODE_WORKTREE_TARGET=worktree,
                    SECODE_WORKTREE_ROOT=checkout)

    def test_controller_swap_preserves_mode_and_leaves_no_partial_on_failure(self):
        from unittest.mock import patch
        source = self.directory/'source.py'
        destination = self.directory/'installed.py'
        source.write_bytes(b'new bytes')
        destination.write_bytes(b'old bytes')
        destination.chmod(0o750)
        install_schedules.atomic_copyfile(source, destination)
        self.assertEqual(destination.read_bytes(), b'new bytes')
        self.assertEqual(destination.stat().st_mode & 0o777, 0o750)
        temporary = destination.with_name(destination.name+'.tmp')
        def interrupted_copy(*_args):
            temporary.write_bytes(b'partial')
            raise OSError('copy interrupted')
        with patch.object(install_schedules.shutil, 'copyfile', side_effect=interrupted_copy):
            with self.assertRaisesRegex(OSError, 'copy interrupted'):
                install_schedules.atomic_copyfile(source, destination)
        self.assertEqual(destination.read_bytes(), b'new bytes')
        self.assertFalse(temporary.exists())

    def test_installed_native_controller_retains_its_reviewed_cleanup_sidecar(self):
        import importlib.util
        control=self.directory/'controller'
        install_schedules.install_control(SCRIPTS.parent,control)
        spec=importlib.util.spec_from_file_location('installed_native_controller',control/'windows_native.py')
        controller=importlib.util.module_from_spec(spec);spec.loader.exec_module(controller)
        self.assertEqual(controller.version_worker().read_bytes(),
                         (SCRIPTS/'release/windows_version_worker.py').read_bytes())

    def test_checkouts_keep_distinct_persistent_targets(self):
        a, b = self.checkout("a"), self.checkout("b")
        a_target, a_full = self.defaults(a)
        b_target, b_full = self.defaults(b)
        self.assertNotEqual(a_target, b_target)
        self.assertEqual(a_full, b_full)
        self.assertEqual(self.defaults(a), [a_target, a_full])
        for target in [a_target, b_target]:
            self.assertEqual(Path(target).parent, Path(a_full) / "worktrees")
            self.assertNotEqual(target, a_full)

    def test_an_outside_target_keeps_no_managed_root_claim(self):
        root = self.checkout('outside-target')
        outside = self.directory/'nvme-targets'/'run-1'
        command = ('source "$1"; printf "%s\\n%s\\n%s\\n" "$CARGO_TARGET_DIR" '
                   '"${SECODE_TARGET_ROOT-unset}" "${SECODE_WORKTREE_TARGET-unset}"')
        for environment, expectation in (
                (dict(self.environment, CARGO_TARGET_DIR=str(outside)), str(outside)),
                (self.environment, None)):
            with self.subTest(outside=expectation is not None):
                result = subprocess.run(['bash','-eu','-c',command,'outside-test',
                                         str(SCRIPTS/'build-cache.sh')],
                                        cwd=root, env=environment, capture_output=True,
                                        text=True, check=True, timeout=20)
                target, managed, worktree = result.stdout.splitlines()
                if expectation is None:
                    # A cache-layout job still gets the claim and its checkout target.
                    self.assertTrue(Path(target).is_relative_to(self.directory/'cache/target'))
                    self.assertEqual(Path(worktree).parent.parent, Path(managed))
                else:
                    # The caller's outside target survives and claims nothing.
                    self.assertEqual(target, expectation)
                    self.assertEqual((managed, worktree), ('unset', 'unset'))

    def test_a_subtree_target_keeps_the_claim(self):
        # A target beneath the managed root is a cache build, so it keeps the
        # claim. Without the subtree branch this silently stops working.
        root = self.checkout('subtree-target')
        _, managed = self.defaults(root)
        toolchain = Path(managed).relative_to(self.directory/'cache/target')
        beneath = self.directory/'cache/target'/toolchain/'worktrees'/'another-checkout'
        for target in (str(beneath), str(beneath)+'/'):
            with self.subTest(target=target):
                self.assertEqual(self.claim_for(root, target), managed)

    def test_a_target_through_a_symlinked_cache_keeps_the_claim(self):
        # The box reaches its cache through a symlink; a target named through it
        # must still be recognised as a cache build.
        link = self.directory/'linked-cache'
        link.symlink_to(self.directory/'cache', target_is_directory=True)
        root = self.checkout('linked-target')
        _, managed = self.defaults(root)
        toolchain = Path(managed).relative_to(self.directory/'cache/target')
        through = link/'target'/toolchain/'worktrees'/'another-checkout'
        self.assertEqual(self.claim_for(root, str(through)), managed)

    def test_a_ram_run_subtree_keeps_the_claim(self):
        # A governed RAM run writes inside its own reservation, which is not under
        # the cache; it still owns that target while the run is live.
        root = self.checkout('ram-target')
        reservation = self.directory/'ram-run'
        reserved = str(reservation/'target'/'worktrees'/'another-checkout')
        self.assertEqual(self.claim_for(root, reserved, SECODE_RAM_TARGET_RUN=str(reservation)),
                         self.defaults(root)[1])
        # The same target without the live reservation has no claim, so the
        # reservation does the work rather than the location.
        self.assertIsNone(self.claim_for(root, reserved))

    def claim_for(self, root, target, **extra):
        command = 'source "$1"; printf "%s" "${SECODE_TARGET_ROOT-unset}"'
        result = subprocess.run(['bash','-eu','-c',command,'claim-for',
                                 str(SCRIPTS/'build-cache.sh')],
                                cwd=root, env=dict(self.environment, CARGO_TARGET_DIR=target, **extra),
                                capture_output=True, text=True, check=True, timeout=20)
        return None if result.stdout == 'unset' else result.stdout

    def test_environment_reader_accepts_an_outside_target_without_a_claim(self):
        root = self.checkout('outside-environment')
        outside = self.directory/'nvme-targets'/'run-2'
        for environment in (dict(self.environment, CARGO_TARGET_DIR=str(outside)),
                            # A stale inherited claim must not survive either.
                            dict(self.environment, CARGO_TARGET_DIR=str(outside),
                                 SECODE_TARGET_ROOT=str(self.directory/'cache/target/stale'),
                                 SECODE_WORKTREE_TARGET=str(self.directory/'cache/target/stale/worktrees/x'))):
            with self.subTest(stale='SECODE_TARGET_ROOT' in environment):
                values = build_environment.load(root, environment)
                self.assertEqual(values['CARGO_TARGET_DIR'], str(outside))
                self.assertNotIn('SECODE_TARGET_ROOT', values)
                for name in ('CARGO_HOME', 'SECODE_CACHE_DIR', 'SECODE_WORKTREE_ROOT', 'SECODE_FIXTURES'):
                    self.assertIn(name, values)

    def test_defaults_disable_compiler_wrapping_and_preserve_checkout_target(self):
        root = self.checkout('fresh-host')
        (self.directory/'fake-tools/sccache').unlink()
        environment = dict(self.environment, HOME=str(self.directory/'home'),
                           PATH=str(self.directory/'fake-tools')+':/usr/bin:/bin', RUSTC_WRAPPER='/foreign/wrapper')
        result = subprocess.run(['bash','-eu','-c',
                                 'source "$1"; printf "%s\\n%s\\n" "${RUSTC_WRAPPER-}" "$CARGO_TARGET_DIR"',
                                 'fresh-host',str(SCRIPTS/'build-cache.sh')],
                                cwd=root,env=environment,check=True,capture_output=True,text=True,timeout=20)
        wrapper, target = result.stdout.splitlines()
        self.assertEqual(wrapper, '')
        self.assertTrue(Path(target).is_relative_to(self.directory/'cache/target'))

    def test_shell_startup_is_project_scoped_and_preserves_admitted_targets(self):
        home = self.directory/'shell-home'; home.mkdir()
        library = home/'.local/lib/secode-release'
        install_schedules.install_control(SCRIPTS.parent, library)
        original = b'return 0\n# User shell settings\n'
        (home/'.bashrc').write_bytes(original)
        install_schedules.install_seat_shells(home)
        installed = (home/'.bashrc').read_bytes()
        self.assertTrue(installed.endswith(original))
        install_schedules.install_seat_shells(home)
        self.assertEqual((home/'.bashrc').read_bytes(), installed)
        for version in ('a'*40,'b'*40):
            snapshot=home/'.local/lib/secode-release-snapshots'/version
            install_schedules.install_control(SCRIPTS.parent,snapshot)
            install_schedules.install_seat_shells(home,snapshot)
            self.assertTrue((home/'.bashrc').read_bytes().endswith(original))
            self.assertIn(str(snapshot/'seat-shell.sh').encode(),(home/'.bashrc').read_bytes())
        self.assertEqual((library/'seat-shell.sh').read_bytes(),(snapshot/'seat-shell.sh').read_bytes())
        root = self.checkout('shell-checkout')
        command = ['bash', '-eu', '-c', 'source "$1"; printf "%s|%s" "${SECODE_CACHE_DIR:-}" "${CARGO_TARGET_DIR:-}"',
                   'shell-test', str(home/'.bashrc')]
        for project, lease, expected in [('secode', '', str(self.directory/'cache')+'|'),
                                          ('other', '', str(self.directory/'cache')+'|preserved'),
                                          ('secode', 'admitted', str(self.directory/'cache')+'|preserved')]:
            environment = dict(self.environment, HOME=str(home), STATEENGINE_PROJECT_ID=project,
                               SECODE_HEAVY_LEASE=lease, CARGO_TARGET_DIR='preserved', CARGO_BUILD_JOBS='8')
            result = subprocess.run(command, cwd=root, env=environment, check=True,
                                    capture_output=True, text=True, timeout=20)
            self.assertEqual(result.stdout, expected)
            if project == 'secode' and not lease:
                jobs = subprocess.run(['bash','-eu','-c','source "$1"; printf "%s" "$CARGO_BUILD_JOBS"',
                                       'shell-jobs',str(home/'.bashrc')], cwd=root, env=environment,
                                      check=True,capture_output=True,text=True,timeout=20)
                self.assertEqual(jobs.stdout, '3')
        from unittest.mock import patch
        create = install_schedules.tempfile.mkstemp
        profile = home/'.bashrc'
        for mutation in ['content', 'identity', 'mode', 'created']:
            with self.subTest(mutation=mutation):
                preserved = []
                profile.write_bytes(original); profile.chmod(0o640)
                if mutation == 'created':
                    profile.unlink()
                def concurrent_edit(*args, **kwargs):
                    result = create(*args, **kwargs)
                    if mutation in {'content', 'created'}:
                        profile.write_bytes(original+b'# Concurrent user line\n')
                    elif mutation == 'identity':
                        replacement = home/'replacement'
                        replacement.write_bytes(original); replacement.chmod(0o640)
                        replacement.replace(profile)
                    else:
                        profile.chmod(0o600)
                    preserved.append((profile.read_bytes(), profile.stat().st_mode & 0o777))
                    return result
                with patch.object(install_schedules.tempfile, 'mkstemp', concurrent_edit):
                    with self.assertRaisesRegex(ValueError, 'changed'):
                        install_schedules.install_seat_shells(home)
                self.assertEqual((profile.read_bytes(), profile.stat().st_mode & 0o777), preserved[0])

    def test_heavy_and_seat_environments_keep_checkout_ownership_without_a_compiler_cache(self):
        import json
        import sys
        root = self.checkout('producer')
        (root/'crates/grapt-types').mkdir(parents=True)
        (root/'crates/grapt-types/Cargo.toml').touch()
        (root/'Cargo.toml').touch()
        environment = dict(self.environment)
        environment.pop('CARGO_BUILD_JOBS', None)
        command = [sys.executable, '-c',
                   'import json,os; print(json.dumps({k:os.environ.get(k) for k in '
                   '["CARGO_TARGET_DIR","SECODE_CACHE_DIR","RUSTC_WRAPPER","CARGO_BUILD_JOBS"]}))']
        from unittest.mock import patch
        # The product's developer-tool checks isolate HOME from the host queue.
        with patch.dict(os.environ, dict(self.environment, HOME=str(self.directory/'process-home')), clear=True):
            with heavy.owned_process(command, cwd=root, env=environment, stdout=subprocess.PIPE, text=True) as process:
                output, _ = process.communicate(timeout=20)
                self.assertEqual(process.returncode, 0)
        values = json.loads(output)
        self.assertEqual(values['CARGO_BUILD_JOBS'], '3')
        self.assertEqual(values['SECODE_CACHE_DIR'], str(self.directory/'cache'))
        target = Path(values['CARGO_TARGET_DIR'])
        self.assertTrue((target/'.secode-worktree').is_file())
        self.assertEqual((target/'.secode-worktree').read_text(), str(root)+'\n')
        other = self.checkout('other-producer')
        (other/'crates/grapt-types').mkdir(parents=True)
        (other/'crates/grapt-types/Cargo.toml').touch(); (other/'Cargo.toml').touch()
        with self.assertRaisesRegex(ValueError, 'another checkout'):
            build_environment.for_checkout(other, dict(environment, CARGO_TARGET_DIR=str(target)))
        first = build_environment.for_checkout(root, environment)
        switched = build_environment.for_checkout(other, first)
        self.assertNotEqual(switched['CARGO_TARGET_DIR'], first['CARGO_TARGET_DIR'])
        self.assertEqual(switched['SECODE_CACHE_DIR'], first['SECODE_CACHE_DIR'])
        script = 'source "$1" "$2"; test -z "${CARGO_TARGET_DIR+x}"; printf "%s" "$SECODE_CACHE_DIR"'
        result = subprocess.run(['bash','-eu','-c',script,'seat-env',str(SCRIPTS/'build/seat-env.sh'),str(root)],
                                cwd=root,env=dict(environment,CARGO_TARGET_DIR=str(target)),
                                check=True,capture_output=True,text=True,timeout=20)
        self.assertEqual(result.stdout, values['SECODE_CACHE_DIR'])
        self.assertIsNone(build_environment.for_checkout(self.checkout('foreign'), environment))
        # A transported controller must use the installed policy rather than
        # asking an older accepted candidate for exports it never implemented.
        import importlib.util
        # Documentation Git operations work even when Cargo defaults cannot load.
        with patch.dict(os.environ,environment,clear=True), \
                patch.object(build_environment, 'for_checkout', side_effect=AssertionError('Loaded Cargo for Git')):
            result = heavy.run(['git', 'rev-parse', '--show-toplevel'], cwd=root,
                env=environment, capture_output=True, check=True, timeout=10)
        self.assertEqual(result.stdout.decode().strip(), str(root))
        home = self.directory/'transport-home'
        library = home/'.local/lib/secode-release'
        install_schedules.install_control(SCRIPTS.parent, library)
        transport = self.directory/'transport'; transport.mkdir()
        module_path = transport/'build_environment.py'
        shutil.copyfile(SCRIPTS/'build/build_environment.py', module_path)
        spec = importlib.util.spec_from_file_location('copied_build_environment', module_path)
        module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
        older = self.checkout('older-transport'); (older/'scripts').mkdir()
        (older/'scripts/build-cache.sh').write_text('return 19\n')
        clean = dict(environment, HOME=str(home)); clean.pop('CARGO_HOME', None)
        with patch.dict(os.environ, {'HOME':str(home)}):
            values = module.load(older, clean)
        self.assertEqual(values['CARGO_BUILD_JOBS'], '3')
        self.assertEqual(values['CARGO_HOME'], str(self.directory/'cache/cargo-home'))
        self.assertNotIn('RUSTC_WRAPPER', values)
        # Preserve the export contract held by an already imported controller.
        legacy_names = ('RUSTC_WRAPPER', 'SECODE_SCCACHE', 'SCCACHE_DIR',
                        'SCCACHE_CACHE_SIZE', 'SCCACHE_SERVER_PORT',
                        'SCCACHE_CLIENT_SIDE', 'SCCACHE_BASEDIRS')
        module.EXPORTS += legacy_names
        install_schedules.install_control(SCRIPTS.parent, library)
        with patch.dict(os.environ, {'HOME': str(home)}):
            legacy_values = module.load(older, clean)
        for name in legacy_names:
            self.assertEqual(legacy_values[name], '')
        self.assertEqual(legacy_values['CARGO_INCREMENTAL'], '1')

    def test_installer_works_from_an_older_checkout_and_preserves_config(self):
        root = self.checkout("older")  # No scripts exist in this checkout.
        command = ["bash", str(SCRIPTS / "build/configure-worktree.sh")]
        first = subprocess.run(command, cwd=root, env=self.environment,
                               capture_output=True, text=True, timeout=20)
        self.assertEqual(first.returncode, 0, first.stderr)
        config = root / ".cargo/config.toml"
        before = config.read_bytes()
        installed = tomllib.loads(before.decode())
        target, full = self.defaults(root)
        self.assertEqual(installed["build"]["target-dir"], target)
        self.assertNotEqual(installed["build"]["target-dir"], full)
        # Real Cargo accepts the config and calls rustc directly, without
        # contacting an installed compiler cache.
        import json
        fake = self.directory/'fake-tools'
        real_rustc = shutil.which('rustc')
        (fake/'rustc').write_text('#!/usr/bin/env bash\nexec '+str(real_rustc)+' "$@"\n')
        (root/'Cargo.toml').write_text('[package]\nname="cache-fixture"\nversion="0.0.0"\nedition="2024"\n')
        (root/'src').mkdir(); (root/'src/lib.rs').write_text('pub fn fixture() {}\n')
        result = subprocess.run([shutil.which('cargo'), 'check', '--offline'],
                                cwd=root, env=self.environment, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(installed['build']['incremental'])
        self.assertNotIn('rustc-wrapper', installed['build'])
        # An older generated config can still select sccache after its caller
        # has cleared RUSTC_WRAPPER. The new shell policy must override it.
        config.write_text(before.decode().replace('[build]\n',
            '[build]\nrustc-wrapper = '+json.dumps(str(fake/'sccache'))+
            '\nrustc-workspace-wrapper = '+json.dumps(str(fake/'sccache'))+'\n',1))
        legacy = subprocess.run(['bash','-eu','-c','source "$1"; cargo check --offline',
                                 'legacy-wrapper',str(SCRIPTS/'build-cache.sh')],
                                cwd=root,env=dict(self.environment,RUSTC_WRAPPER='/foreign/wrapper',
                                         RUSTC_WORKSPACE_WRAPPER=str(fake/'sccache')),
                                capture_output=True,text=True,timeout=30)
        self.assertEqual(legacy.returncode,0,legacy.stderr)
        config.write_bytes(before)
        (root/'crates/grapt-types').mkdir(parents=True)
        (root/'crates/grapt-types/Cargo.toml').touch()
        conflicting = dict(self.environment, RUSTC_WRAPPER='/foreign/wrapper',
                           RUSTC_WORKSPACE_WRAPPER=str(fake/'sccache'))
        loaded = build_environment.for_checkout(root, conflicting)
        self.assertNotIn('RUSTC_WRAPPER', loaded)
        self.assertNotIn('RUSTC_WORKSPACE_WRAPPER', loaded)
        self.assertEqual(loaded['CARGO_INCREMENTAL'], '1')
        startup = subprocess.run(['bash','-eu','-c',
                                 'source "$1" "$2"; test -z "${RUSTC_WRAPPER+x}"; '
                                 'test -z "${RUSTC_WORKSPACE_WRAPPER+x}"; '
                                 'test "$CARGO_INCREMENTAL" = 1',
                                 'cache-seat',str(SCRIPTS/'build/seat-env.sh'),str(root)],
                                cwd=root,env=conflicting,capture_output=True,text=True,timeout=20)
        self.assertEqual(startup.returncode,0,startup.stderr)
        for item in ['Cargo.toml', 'Cargo.lock', 'src/lib.rs', 'crates/grapt-types/Cargo.toml']:
            (root/item).unlink()
        second = subprocess.run(command, cwd=root, env=self.environment,
                                capture_output=True, text=True, timeout=20)
        self.assertNotEqual(second.returncode, 0)
        self.assertEqual(config.read_bytes(), before)
        status = subprocess.check_output(["git", "status", "--porcelain"], cwd=root, timeout=10)
        self.assertEqual(status, b"")

    def test_refresh_accepts_generated_defaults_before_temp_relocation(self):
        root = self.checkout('refresh-prior')
        scratch = self.directory / 'test-scratch'
        environment = dict(self.environment, SECODE_TEST_TMP_BASE=str(scratch))
        command = ['bash', str(SCRIPTS / 'build/configure-worktree.sh')]
        subprocess.run(command, cwd=root, env=environment, check=True,
                       capture_output=True, timeout=20)
        config = root / '.cargo/config.toml'
        self.assertEqual(tomllib.loads(config.read_text())['env']['TMPDIR']['value'],
                         str(scratch))
        previous = ''.join(line for line in config.read_text().splitlines(keepends=True)
                           if not line.startswith('TMPDIR ='))
        config.write_text(previous)
        subprocess.run(command + ['--refresh'], cwd=root, env=environment,
                       check=True, capture_output=True, timeout=20)
        self.assertEqual(tomllib.loads(config.read_text())['env']['TMPDIR']['value'],
                         str(scratch))

    def test_refresh_falls_back_when_generated_scratch_becomes_unavailable(self):
        root = self.checkout('refresh-scratch-fallback')
        scratch = self.directory / 'test-scratch'
        environment = dict(self.environment, SECODE_TEST_TMP_BASE=str(scratch))
        command = ['bash', str(SCRIPTS / 'build/configure-worktree.sh')]
        subprocess.run(command, cwd=root, env=environment, check=True,
                       capture_output=True, timeout=20)
        config = root / '.cargo/config.toml'
        self.assertEqual(tomllib.loads(config.read_text())['env']['TMPDIR']['value'],
                         str(scratch))
        scratch.rmdir()
        scratch.write_text('a file cannot host test scratch')
        result = subprocess.run(command + ['--refresh'], cwd=root, env=environment,
                                capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn('TMPDIR', tomllib.loads(config.read_text())['env'])

    def test_refresh_refuses_customized_test_scratch_entries(self):
        import json
        root = self.checkout('refresh-scratch-custom')
        scratch = self.directory / 'test-scratch'
        environment = dict(self.environment, SECODE_TEST_TMP_BASE=str(scratch))
        command = ['bash', str(SCRIPTS / 'build/configure-worktree.sh')]
        subprocess.run(command, cwd=root, env=environment, check=True,
                       capture_output=True, timeout=20)
        config = root / '.cargo/config.toml'
        current = config.read_text()
        ours = f'TMPDIR = {{ value = {json.dumps(str(scratch))}, force = false }}\n'
        self.assertIn(ours, current)
        cases = [
            f'TMPDIR = {{ value = {json.dumps(str(scratch))}, force = true }}\n',
            f'TMPDIR = {{ value = {json.dumps(str(scratch))}, force = 0 }}\n',
            f'TMPDIR = {{ value = {json.dumps(str(scratch))}, force = false, extra = "x" }}\n',
            f'TMPDIR = {{ value = {json.dumps(str(self.directory / "foreign"))}, force = false }}\n',
        ]
        for replacement in cases:
            customized = current.replace(ours, replacement)
            config.write_text(customized)
            refused = subprocess.run(command + ['--refresh'], cwd=root, env=environment,
                                     capture_output=True, text=True, timeout=20)
            self.assertNotEqual(refused.returncode, 0, replacement)
            self.assertEqual(config.read_text(), customized)

    def test_refresh_replaces_only_unchanged_generated_defaults(self):
        root = self.checkout("refresh")
        command = ["bash", str(SCRIPTS / "build/configure-worktree.sh")]
        subprocess.run(command, cwd=root, env=self.environment, check=True, capture_output=True, timeout=20)
        config = root / ".cargo/config.toml"
        current = config.read_text()
        job_setting = f'jobs = {tomllib.loads(current)["build"]["jobs"]}\n'
        target, full = self.defaults(root)
        original = current.replace('# Local SE Code build defaults.', '# Local SE Code shared build defaults.', 1)
        original = original.replace(target, full, 1)
        original = original.replace(job_setting, 'jobs = 4\n', 1)
        original = original.replace('"100G"', '"20G"')
        original = '\n'.join(line for line in original.split('\n') if not line.startswith('SCCACHE_SERVER_PORT ='))
        config.write_text(original)
        subprocess.run(command + ["--refresh"], cwd=root, env=self.environment,
                       check=True, capture_output=True, timeout=20)
        self.assertEqual(config.read_text(), current)
        # Refresh a previous generated config without requiring Git history.
        import json
        wrapper = json.dumps(str(SCRIPTS/'build/rustc-cache.sh'))
        legacy = current.replace(job_setting, f'rustc-wrapper = {wrapper}\n{job_setting}', 1)
        legacy = legacy.replace('incremental = true', 'incremental = false', 1)
        cache = ''.join(f'{name} = {{ value = {json.dumps(value)}, force = true }}\n'
                        for name, value in [('SCCACHE_DIR', str(self.directory/'cache/sccache')),
                                            ('SCCACHE_CACHE_SIZE', '100G'),
                                            ('SCCACHE_SERVER_PORT', '4226'), ('SCCACHE_CLIENT_SIDE', '1')])
        fixtures = next(line for line in legacy.splitlines() if line.startswith('SECODE_FIXTURES ='))
        legacy = legacy.replace(fixtures+'\n', fixtures+'\n'+cache, 1)
        legacy += f'SECODE_SCCACHE = {{ value = {json.dumps(str(self.directory/"fake-tools/sccache"))}, force = true }}\n'
        config.write_text(legacy)
        subprocess.run(command+['--refresh'], cwd=root, env=self.environment,
                       check=True, capture_output=True, timeout=20)
        self.assertEqual(config.read_text(), current)
        customized = current + '\n[alias]\nquick = "check"\n'
        config.write_text(customized)
        refused = subprocess.run(command + ["--refresh"], cwd=root, env=self.environment,
                                 capture_output=True, text=True, timeout=20)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(config.read_text(), customized)

    def pre_pr_config(self, root):
        """A [env] a config written before the claim left would carry."""
        _, managed = self.defaults(root)
        worktree = next(line.split('"')[1] for line in
                        (root/'.cargo/config.toml').read_text().splitlines()
                        if line.startswith('SECODE_WORKTREE_ROOT ='))
        cache = (root/'.cargo/config.toml').read_text()
        identity = next(line for line in cache.splitlines() if line.startswith('SECODE_WORKTREE_ROOT ='))
        claims = ''.join(f'{name} = {{ value = {json.dumps(value)}, force = true }}\n'
                         for name, value in [('SECODE_TARGET_ROOT', managed),
                                             ('SECODE_WORKTREE_TARGET',
                                              f'{managed}/worktrees/{hashlib.sha256(worktree.encode()).hexdigest()}'),
                                             ('SECODE_WORKTREE_ROOT', worktree)])
        return cache.replace(identity+'\n', claims, 1)

    def test_refresh_replaces_a_config_written_before_the_claim_left(self):
        root = self.checkout('pre-claim')
        command = ['bash', str(SCRIPTS/'build/configure-worktree.sh')]
        subprocess.run(command, cwd=root, env=self.environment, check=True, capture_output=True, timeout=20)
        config = root/'.cargo/config.toml'
        current = config.read_text()
        old = self.pre_pr_config(root)
        self.assertIn('SECODE_TARGET_ROOT =', old)
        for variant in (old, old.replace(next(line for line in old.splitlines()
                                             if line.startswith('TMPDIR ='))+'\n', '')):
            with self.subTest(with_tmpdir='TMPDIR =' in variant):
                config.write_text(variant)
                refreshed = subprocess.run(command+['--refresh'], cwd=root, env=self.environment,
                                           capture_output=True, text=True, timeout=20)
                self.assertEqual(refreshed.returncode, 0, refreshed.stderr)
                installed = tomllib.loads(config.read_text())
                self.assertNotIn('SECODE_TARGET_ROOT', installed['env'])
                self.assertNotIn('SECODE_WORKTREE_TARGET', installed['env'])
                self.assertEqual(config.read_text(), current)

    def test_a_moved_worktree_written_before_the_claim_left_is_rebonded(self):
        root = self.checkout('pre-claim-move')
        command = ['bash', str(SCRIPTS/'build/configure-worktree.sh')]
        subprocess.run(command, cwd=root, env=self.environment, check=True, capture_output=True, timeout=20)
        config = root/'.cargo/config.toml'
        old = self.pre_pr_config(root)
        config.write_text(old)
        moved = self.directory/'pre-claim-moved'
        root.rename(moved)
        subprocess.run(['git', 'init', '-q', str(moved)], check=True, timeout=10)
        refreshed = subprocess.run(command+['--refresh'], cwd=moved, env=self.environment,
                                   capture_output=True, text=True, timeout=20)
        self.assertEqual(refreshed.returncode, 0, refreshed.stderr)
        installed = tomllib.loads((moved/'.cargo/config.toml').read_text())
        self.assertEqual(installed['env']['SECODE_WORKTREE_ROOT']['value'], str(moved))
        self.assertNotIn('SECODE_TARGET_ROOT', installed['env'])

    def test_refresh_after_worktree_move_rebinds_only_generated_defaults(self):
        self.check_relocated_worktree(repair=False)

    def test_refresh_after_cross_filesystem_copy_and_repair(self):
        self.check_relocated_worktree(repair=True)

    def check_relocated_worktree(self, repair):
        main = self.checkout('main')
        files = ['scripts/build-cache.sh', 'scripts/build/configure-worktree.sh',
                 'scripts/build/rustc-cache.sh', 'scripts/build/tmp_scratch.py',
                 'scripts/build/operator_paths.py']
        for name in files:
            destination = main / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(SCRIPTS.parent / name, destination)
        subprocess.run(['git', 'add', '--', *files], cwd=main, check=True, timeout=10)
        subprocess.run(['git', '-c', 'user.name=CacheTest', '-c', 'user.email=cache@localhost',
                        'commit', '-q', '-F', '-', '--', *files], cwd=main,
                       input=b'Isolated relocation fixture\n', check=True, timeout=10)
        before = self.directory / 'before'
        if repair:
            destination = tempfile.TemporaryDirectory(prefix='secode-cache-repair-', dir='/dev/shm')
            self.addCleanup(destination.cleanup)
            moved = Path(destination.name) / 'moved'
            self.assertNotEqual(before.parent.stat().st_dev, moved.parent.stat().st_dev,
                                'Repair regression requires distinct filesystems')
        else:
            moved = self.directory / 'moved'
        subprocess.run(['git', 'worktree', 'add', '-q', '-b', 'lane', str(before)],
                       cwd=main, check=True, timeout=10)
        subprocess.run(['bash', 'scripts/build/configure-worktree.sh'], cwd=before,
                       env=self.environment, check=True, capture_output=True, timeout=20)
        if repair:
            shutil.copytree(before, moved)
            shutil.rmtree(before)
            subprocess.run(['git', 'worktree', 'repair', str(moved)],
                           cwd=main, check=True, capture_output=True, timeout=10)
        else:
            subprocess.run(['git', 'worktree', 'move', str(before), str(moved)],
                           cwd=main, check=True, timeout=10)
        self.assertEqual(subprocess.check_output(['git', 'rev-parse', '--show-toplevel'],
                                                cwd=moved, text=True, timeout=10).strip(), str(moved))
        config = moved / '.cargo/config.toml'
        stale = config.read_text()
        command = ['bash', 'scripts/build/configure-worktree.sh', '--refresh']
        subprocess.run(command, cwd=moved, env=self.environment,
                       check=True, capture_output=True, timeout=20)
        installed = tomllib.loads(config.read_text())
        self.assertEqual(installed['env']['SECODE_WORKTREE_ROOT']['value'], str(moved))
        # The cache ownership claim stays out: only the checkout identity remains.
        self.assertNotIn('SECODE_TARGET_ROOT', installed['env'])
        self.assertNotIn('SECODE_WORKTREE_TARGET', installed['env'])
        self.assertEqual(installed['build']['target-dir'], self.defaults(moved)[0])
        self.assertNotIn('rustc-wrapper', installed['build'])
        customized = stale + '\n[alias]\nquick = "check"\n'
        config.write_text(customized)
        refused = subprocess.run(command, cwd=moved, env=self.environment,
                                 capture_output=True, text=True, timeout=20)
        self.assertNotEqual(refused.returncode, 0)
        self.assertEqual(config.read_text(), customized)

    def test_gate_refuses_an_explicit_global_target(self):
        root = self.checkout("shared")
        _, shared = self.defaults(root)
        self.environment["CARGO_TARGET_DIR"] = shared
        result = subprocess.run(["bash", str(SCRIPTS / "gate.sh"), "affected", "--list"],
                                cwd=SCRIPTS.parent, env=self.environment, capture_output=True,
                                text=True, timeout=20)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Gate refuses a target outside this checkout", result.stderr)

    def test_existing_marker_warns_above_cleanup_target_and_runs_compiler(self):
        root = self.checkout('pressure')
        subprocess.run(['bash', str(SCRIPTS/'build/configure-worktree.sh')], cwd=root,
                       env=self.environment, check=True, capture_output=True, timeout=20)
        config = tomllib.loads((root/'.cargo/config.toml').read_text())
        environment = dict(self.environment, **self.claim(root))
        environment.update({key:item['value'] for key,item in config['env'].items()})
        target = Path(config['build']['target-dir'])
        target.mkdir(parents=True)
        (target/'.secode-worktree').write_text(str(root)+'\n')
        fake = self.directory/'fake-tools'
        # Exercise the configured threshold without allocating a large cache.
        # The compiler writes a sentinel if ever dispatched.
        pressure = evict_targets.CAP_BYTES + 1
        (fake/'du').write_text(f'#!/usr/bin/env bash\nprintf "{pressure} fixture\\n"\n')
        (fake/'du').chmod(0o755)
        sentinel = self.directory/'compiler-ran'
        compiler = fake/'compiler'
        compiler.write_text('#!/usr/bin/env bash\ntouch "$SENTINEL"\n')
        compiler.chmod(0o755)
        environment.update(SENTINEL=str(sentinel))
        result=subprocess.run([str(SCRIPTS/'build/rustc-cache.sh'),str(compiler),'--version'],
                              cwd=root,env=environment,capture_output=True,text=True,timeout=20)
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertIn('WARNING: cache remains above its cleanup target',result.stderr)
        self.assertIn(str(pressure),result.stderr)
        self.assertTrue(sentinel.exists())
        self.assertTrue((target/'.secode-worktree').is_file())
        evict_targets.lease(target).close()

    def test_retention_cli_preserves_outputs_without_qualification(self):
        import sys
        root = self.checkout('retain')
        subprocess.run(['bash', str(SCRIPTS/'build/configure-worktree.sh')], cwd=root,
                       env=self.environment, check=True, capture_output=True, timeout=20)
        config = tomllib.loads((root/'.cargo/config.toml').read_text())
        environment = dict(self.environment, **self.claim(root))
        environment.update({key:item['value'] for key,item in config['env'].items()})
        target = Path(config['build']['target-dir'])
        command = [sys.executable,str(SCRIPTS/'build/evict_targets.py')]
        absent = subprocess.run(command+['--retain'],cwd=root,env=environment,
                                capture_output=True,timeout=10)
        self.assertNotEqual(absent.returncode,0)
        self.assertFalse(target.exists())
        target.mkdir(parents=True)
        (target/'.secode-worktree').write_text(str(root)+'\n')
        (target/'evidence').write_text('unqualified WIP')
        for option, pinned in [('--retain',True),('--release',False)]:
            result = subprocess.run(command+[option],cwd=root,env=environment,
                                    capture_output=True,text=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual((target/'.secode-retain').exists(),pinned)
            self.assertEqual((target/'evidence').read_text(),'unqualified WIP')
            self.assertFalse((target/'.secode-admission').exists())
            evict_targets.lease(target).close()

    def test_ordinary_cargo_inherits_lease_until_process_exit(self):
        root = self.checkout('ordinary')
        target, _ = self.defaults(root)
        binaries = self.directory / 'bin'
        binaries.mkdir()
        cargo = binaries / 'cargo'
        cargo.write_text('#!/usr/bin/python3\nimport time\nprint(\'{"packages":[]}\', flush=True)\ntime.sleep(0.5)\n')
        cargo.chmod(0o755)
        self.environment['PATH'] = str(binaries) + os.pathsep + self.environment['PATH']
        process = subprocess.Popen(['bash', str(SCRIPTS / 'shared-cargo.sh'), '--version'],
                                   cwd=root, env=self.environment, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE)
        try:
            self.assertEqual(process.stdout.readline(), b'{"packages":[]}\n')
            with self.assertRaises(BlockingIOError):
                evict_targets.lease(Path(target))
            self.assertEqual(process.wait(timeout=10), 0)
            self.assertEqual(process.stdout.read(), b'')
            self.assertIn(b'Whole cache: ', process.stderr.read())
            self.assertEqual((Path(target) / '.secode-worktree').read_text(), str(root) + '\n')
            held = evict_targets.lease(Path(target))
            held.close()
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
            process.stdout.close()
            process.stderr.close()


    def test_cache_defaults_prefer_renamed_home_and_fall_back_to_legacy(self):
        command = 'source "$1"; printf "%s" "$SECODE_CACHE_DIR"'
        for name, (existing, expected) in enumerate(
                [(('grapt', 'secode'), 'grapt'), (('secode',), 'secode'), ((), 'grapt')]):
            home = self.directory / f'legacy-home-{name}'
            for directory in existing:
                (home / '.cache' / directory).mkdir(parents=True)
            environment = dict(self.environment, HOME=str(home))
            environment.pop('SECODE_CACHE_DIR', None)
            result = subprocess.run(['bash', '-eu', '-c', command, 'cache-legacy',
                                     str(SCRIPTS / 'build-cache.sh')],
                                    cwd=self.checkout(f'legacy-{name}'), env=environment,
                                    check=True, capture_output=True, text=True, timeout=20)
            self.assertEqual(result.stdout, str(home / '.cache' / expected))


if __name__ == "__main__":
    unittest.main()
