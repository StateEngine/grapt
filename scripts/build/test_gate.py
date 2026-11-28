import importlib.util
import sys
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("gate", Path(__file__).with_name("gate.py"))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


def package(name, dependencies=()):
    return {"id": name, "name": name, "manifest_path": f"/repo/crates/{name}/Cargo.toml",
            "dependencies": [{"name": d} for d in dependencies]}


class AffectedTests(unittest.TestCase):
    def setUp(self):
        self.packages = [package("types"), package("runtime", ["types"]),
                         package("cli", ["runtime"]), package("other")]

    def test_changed_crate_checks_direct_consumers(self):
        self.assertEqual(gate.select(self.packages, {"crates/types/src/deleted.rs"}, Path("/repo")),
                         ["runtime", "types"])

    def test_cross_crate_rename_checks_both_source_and_destination(self):
        import subprocess
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            old = root/'crates/types/src/lib.rs'
            old.parent.mkdir(parents=True)
            old.write_text('pub fn fixture() {}\n')
            subprocess.run(['git', 'add', '--', 'crates/types/src/lib.rs'], cwd=root, check=True)
            subprocess.run(['git', '-c', 'user.name=Gate', '-c', 'user.email=gate@localhost',
                            'commit', '-q', '-F', '-', '--', 'crates/types/src/lib.rs'],
                           cwd=root, input=b'Base fixture\n', check=True)
            base = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root).decode().strip()
            new = root/'crates/other/src/lib.rs'
            new.parent.mkdir(parents=True)
            old.rename(new)
            subprocess.run(['git', 'add', '--', 'crates/types/src/lib.rs', 'crates/other/src/lib.rs'], cwd=root, check=True)
            def output(*command):
                return subprocess.check_output(command, cwd=root)
            with patch.object(gate, 'output', side_effect=output):
                paths = gate.changed_paths(base)
            self.assertEqual(paths, {'crates/types/src/lib.rs', 'crates/other/src/lib.rs'})
            self.assertEqual(gate.select(self.packages, paths, Path('/repo')), ['other', 'runtime', 'types'])

    def test_docs_do_not_rebuild_rust(self):
        self.assertEqual(gate.select(self.packages, {"docs/plan.md"}, Path("/repo")), [])

    def test_lockfile_and_vendored_changes_check_every_member(self):
        for path in ["Cargo.lock", "third-party/regress/src/lib.rs", ".cargo/config.toml"]:
            with self.subTest(path=path):
                self.assertEqual(gate.select(self.packages, {path}, Path("/repo")),
                                 ["cli", "other", "runtime", "types"])

    def test_benchmark_fixtures_do_not_rebuild_product(self):
        self.assertEqual(gate.select(self.packages, {"fixtures/benchmarks/tasks.json"}, Path("/repo")), [])

    def test_renamed_dependency_uses_package_name(self):
        self.packages[1]["dependencies"] = [{"name": "alias", "package": "types"}]
        self.assertIn("runtime", gate.select(self.packages, {"crates/types/src/lib.rs"}, Path("/repo")))

    def test_api_route_changes_include_tui_even_without_a_manifest_dependency(self):
        packages=[package('grapt-api'),package('grapt-client'),package('grapt-tui'),package('unrelated')]
        self.assertEqual(gate.select(packages,{'crates/grapt-api/src/route.rs'},Path('/repo')),
                         ['grapt-api','grapt-tui'])

class ExecutionTests(unittest.TestCase):
    def test_empty_commands_refuse_before_environment_or_process_creation(self):
        import io
        import time
        import heavy
        from unittest.mock import patch
        for command in ([], (), [''], ['   '], '', None):
            with self.subTest(command=command), \
                    patch.object(gate.windows_gate, 'environment') as environment, \
                    patch.object(heavy, 'owned_process') as process:
                with self.assertRaisesRegex(ValueError, 'non-empty executable'):
                    gate.execute(command, io.StringIO(), time.monotonic()+10)
                environment.assert_not_called()
                process.assert_not_called()

    def test_verified_nvme_full_gates_share_the_full_lock(self):
        import fcntl
        import os
        import tempfile
        from unittest.mock import patch
        import heavy
        with tempfile.TemporaryDirectory() as directory:
            lock = Path(directory)/'full'
            with patch.object(gate, 'FULL_LOCK', lock), \
                    patch.object(heavy, 'shared', return_value=True), \
                    patch.object(heavy, 'nvme_target', return_value=Path('/srv/build/target')), \
                    patch.object(gate.ram_targets, 'validate', return_value=False), \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR':'/srv/build/target'}):
                with gate.full_slot(), lock.open('a') as peer:
                    fcntl.flock(peer, fcntl.LOCK_SH | fcntl.LOCK_NB)
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(peer, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def test_ordinary_tui_and_api_changes_never_select_scheduler_helpers(self):
        import gate_plan
        packages = [package('grapt-client'), package('grapt-api'), package('grapt-runtime')]
        for path in ['crates/grapt-client/src/lib.rs', 'crates/grapt-api/src/http1.rs',
                     'crates/grapt-runtime/src/lib.rs', 'docs/heavy.py']:
            for mode in ['affected', 'full']:
                with self.subTest(path=path, mode=mode):
                    commands = gate_plan.commands(mode, packages, {path}, Path('/repo'))
                    self.assertFalse(any('scripts/build' in command for command in commands))
                    self.assertIn([sys.executable, 'scripts/release/licenses.py', 'check'], commands)
                    if path.startswith('crates/'):
                        self.assertTrue(any(command[:2] == ['cargo', 'test'] for command in commands))

    def test_build_control_changes_and_nightly_runs_keep_scheduler_helpers(self):
        import gate_plan
        helper = [sys.executable, '-m', 'unittest', 'discover', '-s', 'scripts/build', '-p', 'test_*.py']
        for path in ['scripts/build/gate.py', 'scripts/build/heavy.py', 'scripts/build/deleted.py',
                     'scripts/land.ps1', 'scripts/gate.sh', 'scripts/build-cache.sh']:
            with self.subTest(path=path):
                commands = gate_plan.commands('full', [], {path}, Path('/repo'))
                self.assertIn(helper, commands)
        for paths in [set(), {'crates/grapt-client/src/lib.rs'}]:
            self.assertIn(helper, gate_plan.commands('full', [], paths, Path('/repo'), nightly=True))

    def test_full_gate_defaults_to_three_hours(self):
        self.assertEqual(gate.parse_args(['full']).timeout, 10800)

    def test_affected_gate_defaults_to_1800_seconds(self):
        self.assertEqual(gate.parse_args(['affected']).timeout, 1800)

    def test_composition_keeps_workspace_tests_and_direct_consumer_clippy_bounded(self):
        import gate_plan
        packages = [package('a'), package('b', ['a']), package('c', ['b'])]
        self.assertEqual(gate.parse_args(['composition', '--log', '/tmp/composition.log']).timeout, 900)
        self.assertEqual(gate_plan.commands('composition', packages, {'crates/a/src/lib.rs'}, Path('/repo')),
                         gate_plan.composition_commands(['a', 'b']))
        self.assertEqual(gate_plan.commands('composition', packages, {'docs/guide.md'}, Path('/repo')),
                         [['cargo', 'check', '--locked', '--workspace', '--tests']])

    def test_tui_composition_keeps_clippy_when_affected_gate_builds_cli(self):
        import gate_plan
        lint = ['cargo', 'clippy', '--locked', '-p', 'grapt-client',
                '--all-targets', '--', '-D', 'warnings']
        affected = [command for command in gate_plan.affected_commands(['grapt-client'])
                    if command[:2] != ['cargo', 'xwin']]
        self.assertEqual(affected[0], lint)
        self.assertEqual(affected[1],
                         ['cargo', 'build', '--locked', '-p', 'grapt-cli', '--bin', 'grapt',
                          '--features', 'grapt-cli/loopback-testkit'])
        self.assertEqual(gate_plan.commands('composition', [package('grapt-client')],
                                           {'crates/grapt-client/src/lib.rs'}, Path('/repo')),
                         [['cargo', 'check', '--locked', '--workspace', '--tests'], lint,
                          *gate_plan.windows_gate.commands(['grapt-client'])])

    def test_composition_log_is_reserved_once_before_ram_or_disk_execution(self):
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'composition.log'
            options = gate.parse_args(['composition', '--log', str(log)])
            with patch.dict(gate.os.environ, {}, clear=True):
                gate.reserve_composition_log(options)
                claim = gate.os.environ[gate.COMPOSITION_RESERVATION]
                with self.assertRaises(FileExistsError):
                    gate.reserve_composition_log(options)
                gate.os.environ.update({gate.COMPOSITION_RESERVATION:claim,
                                        'SECODE_RAM_TARGET_RUN':'owned-child',
                                        'CARGO_TARGET_DIR':directory})
                with patch.object(gate.ram_targets, 'validate', return_value=True):
                    gate.reserve_composition_log(options)
                    self.assertNotIn(gate.COMPOSITION_RESERVATION, gate.os.environ)
                    with self.assertRaises(FileExistsError):
                        gate.reserve_composition_log(options)
                # Disk fallback enters main directly, reusing the same file.
                log.write_text('RAM or disk attempt output\n')
                self.assertEqual(log.read_text(), 'RAM or disk attempt output\n')

    def test_stale_ram_hint_never_reuses_preexisting_composition_evidence(self):
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'composition.log'
            log.write_text('original evidence\n')
            options = gate.parse_args(['composition', '--log', str(log)])
            for claimed in [False, True]:
                environment = {'SECODE_RAM_TARGET_RUN':'stale-child', 'CARGO_TARGET_DIR':directory}
                if claimed:
                    environment[gate.COMPOSITION_RESERVATION] = '[]'
                with patch.dict(gate.os.environ, environment, clear=True), \
                        patch.object(gate.ram_targets, 'validate', return_value=False):
                    with self.assertRaises(FileExistsError):
                        gate.reserve_composition_log(options)
                self.assertEqual(log.read_text(), 'original evidence\n')

    def test_ram_child_refuses_a_replaced_or_already_written_reserved_log(self):
        import tempfile
        from unittest.mock import patch
        for replace in [False, True]:
            with tempfile.TemporaryDirectory() as directory:
                log = Path(directory)/'composition.log'
                options = gate.parse_args(['composition', '--log', str(log)])
                with patch.dict(gate.os.environ, {}, clear=True):
                    gate.reserve_composition_log(options)
                    if replace:
                        log.rename(log.with_suffix('.old'))
                    log.write_text('existing evidence\n')
                    gate.os.environ.update({'SECODE_RAM_TARGET_RUN':'owned-child',
                                            'CARGO_TARGET_DIR':directory})
                    with patch.object(gate.ram_targets, 'validate', return_value=True):
                        with self.assertRaisesRegex(ValueError, 'reservation changed'):
                            gate.reserve_composition_log(options)
                    self.assertEqual(log.read_text(), 'existing evidence\n')

    def test_explicit_timeout_overrides_either_mode(self):
        for mode in ['full', 'affected']:
            with self.subTest(mode=mode):
                self.assertEqual(gate.parse_args([mode, '--timeout', '123']).timeout, 123)

    def test_timeout_must_remain_positive(self):
        import contextlib
        import io
        for mode in ['full', 'affected']:
            for timeout in ['0', '-1']:
                with self.subTest(mode=mode, timeout=timeout), contextlib.redirect_stderr(io.StringIO()):
                    with self.assertRaises(SystemExit) as failure:
                        gate.parse_args([mode, '--timeout', timeout])
                    self.assertEqual(failure.exception.code, 2)

    def test_full_gate_checks_default_feature_test_targets_before_tests(self):
        commands = [command for command in gate.full_commands(['grapt-product', 'grapt-cli'])
                    if command[:2] != ['cargo', 'xwin']]
        runner=next(command for command in commands if command[:3]==['cargo','nextest','run'])
        self.assertLess(commands.index(commands[1]),commands.index(runner))
        self.assertTrue(any('--doc' in command for command in commands))
        self.assertEqual(commands[1], ['cargo', 'check', '--locked', '--workspace', '--tests'])
        self.assertIn('--all-features', commands[0])
        self.assertIn('--features', runner)

    def test_ordinary_handover_checks_remain_scoped_and_run_without_fixture_features(self):
        expected = [['cargo', 'test', '--locked', '-p', 'grapt-product',
                     '--test', 'handover_journey', 'ordinary_binary_ignores_the_fixture',
                     '--no-fail-fast', '--', '--test-threads=4']]
        self.assertEqual([c for c in gate.full_commands([]) if c[:2]==['cargo','test'] and '--test' in c],expected)
        self.assertEqual(gate.test_features.ordinary_commands(['grapt-product']), expected)
        self.assertEqual(gate.test_features.ordinary_commands(['grapt-cli']), [])

    def test_nextest_concurrency_is_bounded_and_product_doctests_remain_serial(self):
        for packages, workspace in [([], True), (['grapt-product'], False),
                                    (['grapt-product', 'grapt-cli'], False)]:
            with self.subTest(packages=packages, workspace=workspace):
                import nextest_gate
                runner,docs=nextest_gate.plan([gate.test_command(packages,workspace=workspace)])
                self.assertEqual(runner[runner.index('--test-threads')+1],'3')
                self.assertEqual(runner[runner.index('--retries')+1],'0')
                self.assertIn('--test-threads=1',docs)
        self.assertIn('--test-threads=4', gate.test_command(['grapt-provider']))
        self.assertIn('--test-threads=4', gate.test_command(['grapt-client']))

    def test_full_gate_builds_the_tui_binary_before_tests(self):
        commands = gate.full_commands(['grapt-product'], tui=True)
        build = ['cargo', 'build', '--locked', '-p', 'grapt-tui', '--bin', 'grapt-tui']
        runner = next(c for c in commands if c[:3] == ['cargo', 'nextest', 'run'])
        self.assertIn(build, commands)
        self.assertLess(commands.index(build), commands.index(runner))
        # A workspace without the TUI package (a fixture) builds no TUI.
        self.assertNotIn(build, gate.full_commands(['grapt-product']))

    def test_full_gate_builds_the_loopback_cli_when_the_workspace_has_it(self):
        build = ['cargo', 'build', '--locked', '-p', 'grapt-cli', '--bin', 'grapt',
                 '--features', 'grapt-cli/loopback-testkit']
        for kwargs in [{}, {'nightly': True}, {'tui': True}]:
            with self.subTest(variant=kwargs):
                commands = gate.full_commands(['grapt-product'], cli=True, **kwargs)
                runner = next(c for c in commands if c[:3] == ['cargo', 'nextest', 'run'])
                self.assertEqual([c for c in commands if c[:5] == build[:5]], [build])
                self.assertLess(commands.index(build), commands.index(runner))
                # A notice-only full gate still builds it.
                self.assertIn(build, gate.full_commands([], cli=True, **kwargs))
        # A workspace without the CLI package (a fixture) plans no CLI build.
        self.assertNotIn(build, gate.full_commands(['grapt-product']))

    def test_full_gate_runs_every_test_feature(self):
        command = gate.test_command(['grapt-product', 'grapt-cli'], workspace=True)
        self.assertIn('--workspace', command)
        self.assertNotIn('-p', command)
        self.assertEqual(set(command[command.index('--features')+1].split(',')), {
            'grapt-cli/loopback-testkit',
            'grapt-connections/loopback-testkit',
            'grapt-instruction/frozen-fixture',
            'grapt-login/loopback-testkit',
            'grapt-mcp-client/loopback-testkit',
            'grapt-plugin/loopback-testkit',
            'grapt-product/checkpoint-fixture',
            'grapt-product/frozen-fixture',
            'grapt-product/handover-crash-fixture',
            'grapt-product/loopback-provider-testkit',
            'grapt-provider/deterministic-testkit',
            'grapt-provider/frozen-fixture',
            'grapt-provider/loopback-testkit',
            'grapt-release/frozen-fixture',
            'grapt-runtime/loopback-testkit',
            'grapt-service/frozen-fixture',
        })
        self.assertLess(command.index('--features'), command.index('--'))
        self.assertIn('--no-fail-fast', command)

    def test_affected_checks_enable_only_selected_packages_test_features(self):
        command = gate.test_command(['grapt-cli', 'grapt-product'])
        self.assertEqual(command[3:7], ['-p', 'grapt-cli', '-p', 'grapt-product'])
        self.assertEqual(set(command[command.index('--features')+1].split(',')), {
            'grapt-cli/loopback-testkit',
            'grapt-product/checkpoint-fixture',
            'grapt-product/frozen-fixture',
            'grapt-product/handover-crash-fixture',
            'grapt-product/loopback-provider-testkit',
        })
        self.assertNotIn('--all-features', command)

    def test_affected_lint_covers_the_feature_gated_test_targets(self):
        commands = [command for command in gate.affected_commands(['grapt-cli', 'grapt-product'])
                    if command[:2] not in (['cargo', 'xwin'], ['cargo', 'build'])
                    and '--doc' not in command]
        lint, tests, ordinary = commands
        self.assertEqual(tests[:3],['cargo','nextest','run'])
        self.assertEqual(lint[3:7], ['-p', 'grapt-cli', '-p', 'grapt-product'])
        self.assertEqual(lint[lint.index('--features')+1],
                         tests[tests.index('--features')+1])
        self.assertIn('grapt-product/checkpoint-fixture', lint[lint.index('--features')+1].split(','))
        self.assertIn('--all-targets', lint)
        self.assertEqual(lint[-3:], ['--', '-D', 'warnings'])
        self.assertNotIn('--all-features', lint)
        self.assertNotIn('--features', ordinary)
        self.assertEqual(ordinary, gate.test_features.ordinary_commands()[0])

    def test_product_only_gate_prepares_shipped_cli_before_journeys(self):
        commands = [command for command in gate.affected_commands(['grapt-product'])
                    if command[:2] != ['cargo', 'xwin']]
        runner=next(c for c in commands if c[:3]==['cargo','nextest','run'])
        self.assertLess(commands.index(commands[1]),commands.index(runner))
        self.assertEqual(commands[1],
                         ['cargo', 'build', '--locked', '-p', 'grapt-cli', '--bin', 'grapt',
                          '--features', 'grapt-cli/loopback-testkit'])
        self.assertEqual(commands[2], ['cargo', 'build', '--locked', '-p', 'grapt-tui', '--bin', 'grapt-tui'])
        self.assertLess(commands.index(commands[2]), commands.index(runner))
        self.assertEqual([runner[i+1] for i,arg in enumerate(runner[:-1]) if arg=='-p'],['grapt-product'])
        self.assertEqual([c for c in commands if c[:2]==['cargo','test'] and '--test' in c],gate.test_features.ordinary_commands(['grapt-product']))

    def test_product_and_tui_gate_still_builds_the_tui_binary(self):
        commands = gate.affected_commands(['grapt-product', 'grapt-tui'])
        build = ['cargo', 'build', '--locked', '-p', 'grapt-tui', '--bin', 'grapt-tui']
        runner = next(c for c in commands if c[:3] == ['cargo', 'nextest', 'run'])
        self.assertIn(build, commands)
        self.assertLess(commands.index(build), commands.index(runner))

    def test_tui_only_gate_prepares_shipped_cli_before_journeys(self):
        commands = [command for command in gate.affected_commands(['grapt-client'])
                    if command[:2] != ['cargo', 'xwin']]
        runner=next(c for c in commands if c[:3]==['cargo','nextest','run'])
        self.assertLess(commands.index(commands[1]),commands.index(runner))
        self.assertEqual(commands[1],
                         ['cargo', 'build', '--locked', '-p', 'grapt-cli', '--bin', 'grapt',
                          '--features', 'grapt-cli/loopback-testkit'])
        self.assertEqual([runner[i+1] for i,arg in enumerate(runner[:-1]) if arg=='-p'],['grapt-client'])
        shared = gate.affected_commands(['grapt-cli', 'grapt-client'])
        self.assertFalse(any(command[1] == 'build' for command in shared))

    def test_provider_only_lint_uses_only_provider_testkit_features(self):
        lint, tests = [command for command in gate.affected_commands(['grapt-provider'])
                       if command[:2] != ['cargo', 'xwin'] and '--doc' not in command]
        expected = {'grapt-provider/deterministic-testkit', 'grapt-provider/frozen-fixture',
                    'grapt-provider/loopback-testkit'}
        self.assertEqual(set(lint[lint.index('--features')+1].split(',')), expected)
        self.assertEqual(set(tests[tests.index('--features')+1].split(',')), expected)
        self.assertNotIn('grapt-product', lint)

    def test_unrelated_affected_lint_keeps_default_feature_selection(self):
        lint, tests = [command for command in gate.affected_commands(['grapt-types'])
                       if command[:2] != ['cargo', 'xwin'] and '--doc' not in command]
        self.assertNotIn('--features', lint)
        self.assertNotIn('--features', tests)
        self.assertNotIn('--workspace', lint)
        self.assertNotIn('--all-features', lint)
        self.assertEqual(lint[3:5], ['-p', 'grapt-types'])

    def test_declared_testkit_and_fixture_features_are_in_gate_policy(self):
        import tomllib
        root = Path(__file__).resolve().parents[2]
        declared = {}
        for manifest in sorted((root/'crates').glob('*/Cargo.toml')):
            document = tomllib.loads(manifest.read_text())
            features = {feature for feature in document.get('features', {})
                        if feature.endswith(('testkit', 'fixture'))}
            if features:
                declared[document['package']['name']] = features
        self.assertEqual(declared, {name: set(features)
                                    for name, features in gate.test_features.FEATURES.items()})

    def test_provider_only_tests_do_not_enable_cli_or_product_features(self):
        command = gate.test_command(['grapt-provider'])
        self.assertEqual(set(command[command.index('--features')+1].split(',')), {
            'grapt-provider/deterministic-testkit', 'grapt-provider/frozen-fixture',
            'grapt-provider/loopback-testkit'})

    def test_unrelated_package_does_not_enable_product_or_its_testkit(self):
        command = gate.test_command(['grapt-types'])
        self.assertNotIn('--features', command)
        self.assertNotIn('grapt-product', command)
        self.assertNotIn('--workspace', command)

    def test_wait_acquires_and_keeps_the_parent_lock_and_has_a_deadline(self):
        import subprocess
        import tempfile
        import time
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory, patch.object(gate, 'FULL_LOCK', Path(directory)/'lock'), \
                patch.dict(gate.os.environ, {}, clear=True):
            path = str(gate.FULL_LOCK)
            # An independent process owns the slot long enough to prove waiting.
            owner = subprocess.Popen(['flock', path, 'sh', '-c', 'echo ready; sleep 0.3'], stdout=subprocess.PIPE)
            try:
                self.assertEqual(owner.stdout.readline(), b'ready\n')
                began = time.monotonic()
                held = gate.full_slot(wait=2)
                try:
                    self.assertGreater(time.monotonic()-began, 0.15)
                    with self.assertRaisesRegex(gate.GateSkipped, 'slot busy'): gate.full_slot(wait=0.1)
                    with self.assertRaises(RuntimeError): gate.full_slot()
                finally: held.close()
                next_slot = gate.full_slot(); next_slot.close()
            finally:
                owner.wait(timeout=3); owner.stdout.close()

    def test_target_alias_cannot_be_claimed_by_another_checkout(self):
        import tempfile
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            gate.claim_target(target, root / "a")
            gate.claim_target(target, root / "a")
            alias = root / "alias"
            alias.symlink_to(target, target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "owned by another checkout"):
                gate.claim_target(alias, root / "b")

    def test_command_elapsed_preserves_wait_and_failure(self):
        import io
        import subprocess
        from unittest.mock import Mock, patch
        import heavy
        for code in [0, 7]:
            process = Mock()
            process.wait.return_value = code
            log = io.StringIO()
            with patch.object(gate.time, "monotonic", side_effect=[10, 12.5]), patch.object(
                    heavy, "UnreapedProcess", return_value=process) as launch, patch.object(
                    heavy, 'stop_group'), patch.object(heavy, 'wait_owned',
                    side_effect=lambda child, timeout: child.wait(timeout=timeout)):
                if code:
                    with self.assertRaises(subprocess.CalledProcessError) as failure:
                        gate.execute(["fixture"], log, 20, cwd="/fixture")
                    self.assertEqual(failure.exception.returncode, 7)
                else:
                    self.assertEqual(gate.execute(["fixture"], log, 20, cwd="/fixture"), 2.5)
            process.wait.assert_called_once_with(timeout=10)
            launch.assert_called_once()
            self.assertEqual(launch.call_args.args[0][-1], 'fixture')
            self.assertEqual(launch.call_args.kwargs, dict(cwd='/fixture', stdout=log,
                             stderr=subprocess.STDOUT, start_new_session=True))
            self.assertEqual(log.getvalue(), "# command_seconds: 2.500000\n")

    def test_fixture_suite_drops_ownership_claims_but_keeps_kernel_lease_and_scratch(self):
        import io
        import os
        import sys
        import time
        import heavy
        from unittest.mock import Mock, patch
        for command in ([sys.executable, '-m', 'unittest', 'discover'], ['fixture']):
            process = Mock()
            process.wait.return_value = 0
            environment = {heavy.CLAIM:'outer-lease', 'SECODE_RAM_TARGET_RUN':'outer-run',
                           'TMPDIR':'/owned/ram/scratch'}
            with patch.dict(os.environ, environment, clear=True), \
                    patch.object(heavy, 'inherited_fds', return_value=(23,)), \
                    patch('build_environment.for_checkout', return_value=None), \
                    patch.object(heavy, 'UnreapedProcess', return_value=process) as launch, \
                    patch.object(heavy, 'stop_group'):
                gate.execute(command, io.StringIO(), time.monotonic()+10)
                self.assertEqual(dict(os.environ), environment)
            self.assertEqual(launch.call_args.kwargs['pass_fds'], (23,))
            if command[-1] == 'discover':
                self.assertEqual(launch.call_args.kwargs['env'], {'TMPDIR':'/owned/ram/scratch'})
            else:
                self.assertNotIn('env', launch.call_args.kwargs)

    def test_failed_command_cannot_pass(self):
        import subprocess
        import tempfile
        import time
        import sys
        with tempfile.TemporaryFile(mode="w+") as log:
            with self.assertRaises(subprocess.CalledProcessError):
                gate.execute([sys.executable, "-c", "raise SystemExit(7)"], log, time.monotonic()+10)

    def test_timeout_stops_command(self):
        import tempfile
        import time
        import sys
        with tempfile.TemporaryFile(mode="w+") as log:
            with self.assertRaisesRegex(gate.GateTimeout, "timed out"):
                gate.execute([sys.executable, "-c", "import time; time.sleep(20)"], log, time.monotonic()+0.1)

    def test_timeout_kills_term_ignoring_descendant_after_leader_exits(self):
        import heavy
        import os
        import signal
        import sys
        import tempfile
        import time
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory, tempfile.TemporaryFile(mode="w+") as log:
            pidfile = Path(directory)/"child"
            child = ("import os,signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); "
                     f"open({str(pidfile)!r},'w').write(str(os.getpid())+' '+str(os.getpgrp())); "
                     "time.sleep(60)")
            parent = f"import subprocess,sys,time; subprocess.Popen([sys.executable,'-c',{child!r}]); time.sleep(60)"
            try:
                with patch.object(heavy, "STOP_GRACE_SECONDS", 0.2):
                    with self.assertRaisesRegex(gate.GateTimeout, "timed out"):
                        gate.execute([sys.executable, "-c", parent], log, time.monotonic()+0.5)
                pid, group = map(int, pidfile.read_text().split())
                state = Path(f"/proc/{pid}/stat")
                deadline = time.monotonic()+2
                while state.exists() and time.monotonic() < deadline:
                    if state.read_text().split(") ", 1)[1].split()[0] == "Z":
                        break  # A killed child awaiting reaping cannot keep executing.
                    time.sleep(0.02)
                if state.exists():
                    self.assertEqual(state.read_text().split(") ", 1)[1].split()[0], "Z")
            finally:
                if pidfile.exists():
                    _, group = map(int, pidfile.read_text().split())
                    try:
                        os.killpg(group, signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_one_host_lock_refuses_a_second_run(self):
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory, patch.object(gate, "FULL_LOCK", Path(directory)/"full.lock"), \
                patch.dict(gate.os.environ, {}, clear=True):
            a = gate.full_slot()
            try:
                with self.assertRaisesRegex(RuntimeError, 'slot busy'):
                    gate.full_slot()
            finally:
                a.close()
            b = gate.full_slot()
            b.close()

    def test_shared_full_lock_requires_both_scheduler_ram_and_valid_governor(self):
        import tempfile
        import heavy
        from unittest.mock import patch
        # The NVMe target is pinned out: this case asserts the
        # RAM-validator path, so the scratch disk must not qualify it.
        with tempfile.TemporaryDirectory() as directory, patch.object(gate, 'FULL_LOCK', Path(directory)/'lock'), \
                patch.dict(gate.os.environ, {'CARGO_TARGET_DIR':directory}, clear=True), \
                patch.object(heavy, 'shared', return_value=True), \
                patch.object(heavy, 'nvme_target', return_value=None), \
                patch.object(gate.ram_targets, 'validate', return_value=True) as validate:
            first = gate.full_slot()
            second = gate.full_slot(wait=1)
            try:
                self.assertTrue(gate.parse_args(['full', '--exclusive-full']).exclusive_full)
                with self.assertRaisesRegex(RuntimeError, 'slot busy'):
                    gate.full_slot(exclusive=True)
                validate.return_value = False
                with self.assertRaisesRegex(RuntimeError, 'slot busy'):
                    gate.full_slot()
            finally:
                second.close()
                first.close()
            validate.return_value = True
            with gate.full_slot(exclusive=True), self.assertRaisesRegex(RuntimeError, 'slot busy'):
                gate.full_slot()
            # A legacy exclusive waiter must fence new readers while the
            # already admitted readers finish. Use the real kernel wait queue.
            import subprocess
            import time
            import fcntl
            # A timed util-linux waiter re-enters the blocking syscall on
            # every timer signal, and the kernel drops its /proc/locks entry
            # for the gap between attempts; a plain blocking waiter stays in
            # the queue continuously, so the fence sees it on every read.
            # A loaded box starts the fixture interpreter later, so size the
            # queue-reach deadline from a measured startup.
            startup = self.interpreter_startup()
            with gate.full_slot() as admitted, gate.FULL_LOCK.open('a') as descriptor:
                waiter = ("import fcntl,os,sys\n"
                          "fcntl.flock(int(sys.argv[1]), fcntl.LOCK_EX)\n"
                          "os._exit(0)")
                with subprocess.Popen([sys.executable, '-B', '-c', waiter,
                                       str(descriptor.fileno())],
                                      pass_fds=(descriptor.fileno(),)) as writer:
                    try:
                        identity = ':'+str(gate.FULL_LOCK.stat().st_ino)+' '
                        deadline = time.monotonic()+max(2.0, startup*8)
                        while time.monotonic() < deadline:
                            if any('-> FLOCK' in row and 'WRITE '+str(writer.pid)+' ' in row
                                   and identity in row for row in Path('/proc/locks').read_text().splitlines()):
                                break
                            time.sleep(.01)
                        else:
                            self.fail('Exclusive waiter never reached the kernel queue')
                        for wait in (0, .05):
                            with self.assertRaisesRegex(gate.GateSkipped, 'slot busy'):
                                gate.full_slot(wait=wait)
                        admitted.close()
                        self.assertEqual(writer.wait(timeout=2), 0)
                        with self.assertRaisesRegex(gate.GateSkipped, 'slot busy'):
                            gate.full_slot()
                        fcntl.flock(descriptor, fcntl.LOCK_UN)
                    finally:
                        if writer.poll() is None:
                            writer.kill()
                            writer.wait(timeout=2)
            with gate.full_slot(), gate.full_slot():
                pass  # Cancellation/release leaves ordinary shared admission open.

    def test_slow_startup_sample_keeps_legacy_writer_fence_deterministic(self):
        from unittest.mock import patch
        with patch.object(type(self), 'interpreter_startup', return_value=.5, create=True):
            self.test_shared_full_lock_requires_both_scheduler_ram_and_valid_governor()

    @staticmethod
    def interpreter_startup():
        import subprocess
        import time
        began = time.monotonic()
        subprocess.run([sys.executable, '-B', '-c', 'pass'], check=True)
        return max(.05, time.monotonic() - began)



    def test_discovery_keeps_wrapper_private_descriptors_closed(self):
        import os
        import sys
        import tempfile
        import time
        import heavy
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            # A startup adapter owns a descriptor that must not cross exec.
            (root/'sitecustomize.py').write_text(
                "import os,sys\n"
                "if sys.argv[0].endswith('signal_trace.py'):\n"
                " descriptor=os.open(os.devnull,os.O_RDONLY)\n"
                " os.dup2(descriptor,900,inheritable=False)\n"
                " if descriptor!=900:os.close(descriptor)\n")
            (root/'test_private_descriptor.py').write_text(
                "import errno,os,unittest\n"
                "class Discovery(unittest.TestCase):\n"
                " def test_private_descriptor_is_closed(self):\n"
                "  try:os.fstat(900)\n"
                "  except OSError as error:self.assertEqual(error.errno,errno.EBADF)\n"
                "  else:self.fail('wrapper-private descriptor crossed exec')\n")
            with (root/'discovery.log').open('w') as log, \
                    patch.dict(os.environ, {'PYTHONPATH': str(root)}), \
                    patch.object(heavy, 'QUEUE_PATH', root/'queue'):
                gate.execute([sys.executable, '-m', 'unittest', 'discover',
                              '-s', str(root)], log, time.monotonic()+10)
            self.assertIn('Ran 1 test', (root/'discovery.log').read_text())


if __name__ == "__main__":
    unittest.main()


class HelperOnlyGateTests(unittest.TestCase):
    def test_docs_gate_still_runs_the_three_law_checks(self):
        import contextlib
        import io
        import os
        import tempfile
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary)
            log=root/'gate.log'
            args=gate.parse_args(['affected','--base','base','--log',str(log)])
            target=root/'target'; target.mkdir()
            held=(root/'lease').open('w')
            descriptor=held.fileno()
            calls=[]
            def output(*command):
                calls.append(command)
                if command == ('git','rev-parse','--show-toplevel'):
                    return str(root).encode()
                if command == ('git','rev-parse','--verify','base^{commit}'):
                    return b'base-sha'
                if command[:2] == ('cargo','metadata'):
                    return b'{"packages":[],"workspace_members":[]}'
                self.fail(f'Unexpected command: {command}')
            with patch.object(gate,'parse_args',return_value=args), \
                 patch.object(gate,'output',side_effect=output), \
                 patch.object(gate,'changed_paths',return_value={'docs/HOSTED-CI.md'}), \
                 patch.object(gate.publication_scan,'check',return_value=[]), \
                 patch.object(gate.evict_targets,'prepare',return_value=held) as prepare, \
                 patch.object(gate.evict_targets,'retain') as retain, \
                 patch.object(gate.evict_targets,'inherit'), \
                 patch.object(gate,'claim_target'), \
                 patch.dict(os.environ, {'SECODE_TARGET_ROOT':str(root),'CARGO_TARGET_DIR':str(target)}), \
                 patch.object(gate,'execute',return_value=0.1) as execute, \
                 patch.object(gate.receipt,'source',return_value={}), \
                 patch.object(gate.receipt,'write') as write, \
                 patch.object(gate.signal,'signal'), \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(gate.main(),0)
            prepare.assert_called_once()
            retain.assert_called_once()
            self.assertEqual(execute.call_count,4)
            self.assertEqual(execute.call_args_list[0].args[0][1:],
                             ['scripts/release/licenses.py', 'check'])
            self.assertEqual(execute.call_args_list[1].args[0][1:],
                             ['scripts/build/ratchet_monotonic.py', 'base-sha'])
            self.assertEqual([[arg for arg in call.args[0] if not arg.startswith('--message-format=')]
                              for call in execute.call_args_list[2:]],gate.gate_plan.law_commands())
            self.assertEqual(execute.call_args.kwargs['lease_fd'],descriptor)
            write.assert_called_once()


    def test_gate_commands_inherit_the_retained_lease(self):
        import sys
        import tempfile
        import time
        with tempfile.TemporaryDirectory() as temporary:
            directory=Path(temporary)
            with (directory/'lease').open('w') as held, (directory/'log').open('w') as log:
                gate.execute([sys.executable,'-c',
                              'import os,sys; os.fstat(int(sys.argv[1])); print("lease inherited")',
                              str(held.fileno())],log,time.monotonic()+5,lease_fd=held.fileno())
            self.assertIn('lease inherited',(directory/'log').read_text())
