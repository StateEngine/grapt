"""Foreign Cargo attribution cannot evict a target it might still use."""
import hashlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from fixture_environment import isolated_environment
import cargo_targets
import cargo_observation
import disk_hygiene
import evict_targets


class CargoTargetTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.home = self.root / 'home'
        self.home.mkdir()
        self.cache = self.root / 'cache'
        self.cwd = self.root / 'foreign'
        self.cwd.mkdir()
        (self.cwd / 'Cargo.toml').write_text(
            '[package]\nname="foreign-fixture"\nversion="0.1.0"\nedition="2021"\n')
        (self.cwd / 'src').mkdir()
        (self.cwd / 'src/lib.rs').write_text('pub fn fixture() {}\n')
        self.environment = {b'HOME': os.fsencode(self.home)}
        self.now = time.time()
        self.process_count = 0

    def resolve(self, args=(), environment=None, cwd=None):
        return cargo_targets.resolve(cwd or self.cwd, environment or self.environment,
                                     [b'cargo', b'check', *[os.fsencode(a) for a in args]],
                                     time.monotonic() + 10)

    def config(self, directory, text, name='config.toml'):
        directory.mkdir(parents=True, exist_ok=True)
        (directory / name).write_text(text)

    def target(self, name='owner'):
        owner = self.root / name
        owner.mkdir()
        (owner / 'source.rs').write_text('keep')
        target = (self.cache / 'target/toolchain/worktrees' /
                  hashlib.sha256(str(owner).encode()).hexdigest())
        (target / 'debug').mkdir(parents=True)
        (target / '.secode-worktree').write_text(str(owner) + '\n')
        (target / 'debug/artifact').write_bytes(b'compiled')
        for path in [*target.rglob('*'), target]:
            os.utime(path, (self.now - 7200, self.now - 7200))
        return target

    def process(self, target=None, command='check', extra_env=None, descendant=True):
        """A synthetic live Cargo, with no real process behind it.

        Everything the censuses read is a plain file or directory in this
        fixture: stat, status, comm, cmdline, environ, cwd, exe, fd and task.
        Nothing points into the real /proc, so the result is the same on any
        host. A test that needs a build in flight gets this entry, never Cargo.
        """
        import cargo_observation
        self.process_count += 1
        pid = str(4000 + self.process_count)
        proc = self.root / f'proc-{self.process_count}'
        live = proc / pid
        (live / 'fd').mkdir(parents=True)
        (live / 'task' / pid).mkdir(parents=True)
        # A real start time keeps the PID stability check happy.
        (live / 'stat').write_bytes(
            pid.encode() + b' (cargo) S ' + b'0 ' * 18 + b'99\n')
        (live / 'task' / pid / 'children').write_text('')
        (live / 'comm').write_text('cargo\n')
        (live / 'exe').symlink_to('/fixture/cargo')
        (live / 'cwd').symlink_to(self.cwd, target_is_directory=True)
        environment = {b'HOME': os.fsencode(self.home)}
        environment.update({os.fsencode(k): os.fsencode(v)
                            for k, v in (extra_env or {}).items()})
        if target is not None:
            environment[b'CARGO_TARGET_DIR'] = os.fsencode(target)
        (live / 'environ').write_bytes(
            b'\0'.join(key + b'=' + value for key, value in environment.items()) + b'\0')
        (live / 'cmdline').write_bytes(
            b'cargo\0' + os.fsencode(command) + b'\0--offline\0')
        # A descriptor into the source checkout, as a real build holds.
        (live / 'fd' / '3').symlink_to(self.cwd / 'Cargo.toml')
        self.descendant = None
        if descendant:
            child = str(5000 + self.process_count)
            kid = proc / child
            (kid / 'fd').mkdir(parents=True)
            (kid / 'task' / child).mkdir(parents=True)
            (kid / 'stat').write_bytes(child.encode() + b' (rustc) S ' + b'0 ' * 18 + b'99\n')
            (kid / 'task' / child / 'children').write_text('')
            (kid / 'comm').write_text('rustc\n')
            (kid / 'exe').symlink_to('/fixture/rustc')
            (kid / 'cwd').symlink_to(self.cwd, target_is_directory=True)
            (kid / 'environ').write_bytes(b'HOME=' + os.fsencode(self.home) + b'\0')
            (kid / 'cmdline').write_bytes(b'rustc\0--out-dir\0outside\0')
            (live / 'task' / pid / 'children').write_text(child)
            self.descendant = child
        return proc

    def reclaim(self, proc, queue=None):
        """Run a pass over this fixture's process table and nothing else.

        No scanner is stubbed: the real functions run against the injected
        table, so a path that forgot to thread proc fails with TypeError
        instead of being rescued by a mock. The queue file is injected too, so
        identity comes from the fixture rather than the host.
        """
        with patch.object(evict_targets, 'HEAVY_QUEUE',
                          queue if queue is not None else self.root/'absent-queue.json'), \
                patch.object(disk_hygiene.ram_targets, 'reap', return_value=[]), \
                patch.object(disk_hygiene.clean_journal, 'journal_path',
                             lambda directory=None: self.root/'journal.json'), \
                patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}):
            return disk_hygiene.reclaim(self.cache, self.root, now=self.now, proc=proc)

    def assert_refused_on_merits(self, proc, owners=None):
        """The pass refuses, and the refusal carries observed evidence.

        Evidence is what the census itself saw: which process it could not read,
        which process named a reference, or which output was unattributable.
        Every recorder wraps a real function, so a forced-blind census fires no
        veto, carries no evidence, and fails here. A label the test supplies is
        never evidence.
        """
        evidence = []
        import evict_targets as targets_module
        original_read = cargo_observation.read
        first = cargo_observation.foreign
        second = cargo_targets.foreign

        def recording_read(path, deadline):
            try:
                return original_read(path, deadline)
            except (OSError, ValueError) as failure:
                evidence.append(('unreadable', Path(path).parent.name, Path(path).name,
                                 type(failure).__name__))
                raise

        def recording_first(process, proc_root, roots_, deadline, excused=None, **more):
            try:
                verdict = first(process, proc_root, roots_, deadline, excused=excused, **more)
            except (OSError, ValueError) as failure:
                evidence.append(('unreadable', process.name, 'union',
                                 type(failure).__name__))
                raise
            if verdict is not True:
                evidence.append(('reference', process.name))
            return verdict

        def recording_second(cwd, environment, arguments, owners_, deadline, kind='cargo',
                             excused=None, **more):
            try:
                verdict = second(cwd, environment, arguments, owners_, deadline, kind=kind,
                                 excused=excused, **more)
            except (OSError, ValueError) as failure:
                evidence.append(('unattributable', kind, type(failure).__name__))
                raise
            if not verdict:
                evidence.append(('output', str(cwd)))
            return verdict
        with patch.object(cargo_observation, 'read', side_effect=recording_read), \
                patch.object(cargo_observation, 'foreign', side_effect=recording_first), \
                patch.object(cargo_targets, 'foreign', side_effect=recording_second):
            with self.assertRaisesRegex(RuntimeError, 'Unknown live Cargo'):
                self.reclaim(proc)
        self.assertTrue(evidence,
                        'the pass refused with no evidence: no veto fired and nothing was '
                        'unreadable, so the refusal was the census reading nothing')
        return evidence

    def refusal_reason(self, proc, owners=None):
        """Why live_targets refuses this fixture, or None when it did not.

        A refusal is only evidence when a process or token caused it. Reading a
        process the scanner cannot prove anything about is not a refusal on the
        merits, so it reports 'blindness' and never satisfies a refusal test.
        """
        recorded = []
        original = cargo_observation.foreign
        second = cargo_targets.foreign

        def recording(process, proc_root, roots_, deadline, excused=None, **more):
            try:
                verdict = original(process, proc_root, roots_, deadline, excused=excused, **more)
            except (OSError, ValueError, RuntimeError) as failure:
                recorded.append(('unreadable', process.name, type(failure).__name__))
                raise
            if verdict is not True:
                recorded.append((process.name, excused))
            return verdict

        def recording_second(cwd, environment, arguments, owners_, deadline, kind='cargo',
                             excused=None, **more):
            verdict = second(cwd, environment, arguments, owners_, deadline, kind=kind,
                             excused=excused, **more)
            if not verdict:
                recorded.append(('output', str(cwd), kind))
            return verdict
        with patch.object(cargo_observation, 'foreign', side_effect=recording), \
                patch.object(cargo_targets, 'foreign', side_effect=recording_second):
            verdict = evict_targets.live_targets(owners or {}, proc,
                                                 deadline=time.monotonic() + 30)
        if verdict is not None:
            return None
        if recorded:
            return recorded
        # Nothing was recorded, so the refusal happened outside both vetoes. Name
        # which census went dark rather than leaving it unexplained.
        with patch.object(cargo_observation, 'foreign', side_effect=recording), \
                patch.object(cargo_targets, 'foreign', side_effect=recording_second):
            builds = evict_targets.live_targets(owners or {}, proc,
                                                deadline=time.monotonic() + 30)
        if builds is None and not recorded:
            return 'blindness'
        return recorded or 'blindness'

    def build(self, environ=(), arguments=(), descriptors=()):
        """A synthetic live Cargo in a private /proc tree, for exact control."""
        self.process_count += 1
        pid = str(1000 + self.process_count)
        proc = self.root / f'synthetic-proc-{self.process_count}'
        process = proc / pid
        (process / 'fd').mkdir(parents=True)
        (process / 'task' / pid).mkdir(parents=True)
        (process / 'stat').write_bytes(pid.encode() + b' (cargo) R ' + b'0 ' * 18 + b'1\n')
        (process / 'task' / pid / 'children').write_text('')
        (process / 'comm').write_text('cargo\n')
        (process / 'exe').symlink_to(os.environ.get('CARGO', '/usr/bin/false'))
        (process / 'cwd').symlink_to(self.cwd, target_is_directory=True)
        (process / 'cmdline').write_bytes(b'cargo\0check\0' + b'\0'.join(
            [os.fsencode(value) for value in arguments]) + b'\0')
        (process / 'environ').write_bytes(b'\0'.join(
            [os.fsencode(value) for value in environ]) + b'\0')
        for name, target in descriptors:
            (process / 'fd' / name).symlink_to(target)
        return proc

    def test_a_live_build_protects_its_own_target_and_the_pass_continues(self):
        own = self.target('own-owner')
        other = self.target('other-owner')
        proc = self.build(['HOME=' + str(self.home), 'CARGO_TARGET_DIR=' + str(own)])
        self.assertIn(other, self.reclaim(proc), 'every other idle target is reclaimed')
        self.assertTrue(own.exists(), 'a live build keeps its own output')

    def test_no_census_function_defaults_its_process_table(self):
        """A default proc is how a missing fixture hides: the call silently reads
        the host. Every census and reclaim function must take proc, and none may
        supply a default."""
        import duplicate_targets
        import evict_targets
        import inspect
        import retention
        functions = [
            disk_hygiene.reclaim, disk_hygiene.active_targets,
            disk_hygiene.reclaim_owned_targets, disk_hygiene.scheduled_reclaim,
            evict_targets.live_targets, evict_targets.evict, evict_targets.prepare,
            evict_targets.process_identity, evict_targets.queued_targets,
            duplicate_targets.prune_owned, disk_hygiene.orphan_cache.active,
            disk_hygiene.orphan_cache.reclaim, retention.unused,
            retention.reclaim_scratch, retention.archive_worktree,
        ]
        for function in functions:
            parameters = inspect.signature(function).parameters
            table = None
            for name, parameter in parameters.items():
                if name in ('proc', 'proc_root'):
                    table = parameter
                    break
            self.assertIsNotNone(table, f'{function.__module__}.{function.__name__} '
                                         'must take its process table explicitly')
            self.assertIs(table.default, inspect.Parameter.empty,
                          f'{function.__module__}.{function.__name__} must not default '
                          'its process table: a missing fixture must fail to call')

    def test_no_census_can_reach_the_host_process_table(self):
        # The whole point of the fixture: a path that forgets to pass this
        # table must fail to call, not quietly read the host. live_targets and
        # active() take proc with no default, so the omission is a TypeError.
        with self.assertRaises(TypeError):
            evict_targets.live_targets({}, deadline=time.monotonic() + 5)
        with self.assertRaises(TypeError):
            disk_hygiene.orphan_cache.active([self.cache], time.monotonic() + 5)
        with self.assertRaises(TypeError):
            disk_hygiene.scheduled_reclaim(self.cache, self.root, False, None)

    def test_a_blind_census_never_satisfies_a_refusal_test(self):
        # The defect the hosted runner exposed. On main this pattern passed
        # vacuously: the census read the host's /proc, a non-dumpable process
        # blinded it, live_targets returned None, and the refusal test was
        # satisfied by the blindness rather than by the reference it names.
        own = self.target('own-owner')
        third = self.target('third-owner')
        proc = self.build(['HOME=' + str(self.home), 'CARGO_TARGET_DIR=' + str(own),
                           'CUSTOM_PATH=' + str(third / 'debug/artifact')])
        # A refusal on the merits names the process and token that caused it.
        reasons = self.refusal_reason(proc, {own: self.root / 'own-owner',
                                             third: self.root / 'third-owner'})
        self.assertTrue(reasons, 'a build holding a second target must refuse')
        self.assertNotEqual(reasons, 'blindness',
                            'a refusal must name its cause, never the census going dark')
        # Forced blindness produces the vacuous shape, which this assertion
        # rejects: that is what makes the test fail where main passed.
        with patch.object(evict_targets, 'live_targets', return_value=None):
            self.assertEqual(self.refusal_reason(proc), 'blindness')
            with self.assertRaises(AssertionError):
                self.assertNotEqual(self.refusal_reason(proc), 'blindness',
                                    'a refusal must name its cause')

    def test_a_live_build_holding_a_second_target_still_refuses_all_eviction(self):
        own = self.target('own-owner')
        other = self.target('other-owner')
        proc = self.build(['HOME=' + str(self.home), 'CARGO_TARGET_DIR=' + str(own)],
                          descriptors=[('9', other / 'debug/artifact')])
        self.assert_refused_on_merits(proc)
        self.assertTrue(own.exists())
        self.assertTrue(other.exists())

    def test_an_alias_spelling_of_the_own_target_resolves_to_the_same_root(self):
        own = self.target('own-owner')
        other = self.target('other-owner')
        alias = self.root / 'own-alias'
        alias.symlink_to(own, target_is_directory=True)
        proc = self.build(['HOME=' + str(self.home), 'CARGO_TARGET_DIR=' + str(alias)])
        self.assertIn(other, self.reclaim(proc))
        self.assertTrue(own.exists())

    def test_an_unresolvable_own_target_still_refuses_all_eviction(self):
        own = self.target('own-owner')
        missing = self.root / 'absent-own-target'
        proc = self.build(['HOME=' + str(self.home), 'CARGO_TARGET_DIR=' + str(missing)])
        self.assert_refused_on_merits(proc)
        self.assertTrue(own.exists())

    def test_two_disagreeing_own_target_values_exempt_neither(self):
        first = self.target('own-owner')
        second = self.target('other-owner')
        proc = self.build(['HOME=' + str(self.home), 'CARGO_TARGET_DIR=' + str(first)],
                          arguments=['--target-dir', str(second)])
        self.assert_refused_on_merits(proc)
        self.assertTrue(first.exists())
        self.assertTrue(second.exists())

    def test_review_nested_own_output_protects_its_enclosing_target(self):
        enclosing = self.target('own-owner')
        other = self.target('other-owner')
        nested = enclosing / 'debug/nested-target'
        nested.mkdir()
        for path in [*enclosing.rglob('*'), enclosing]:
            os.utime(path, (self.now - 7200, self.now - 7200))
        proc = self.build(['HOME=' + str(self.home), 'CARGO_TARGET_DIR=' + str(nested)])
        removed = self.reclaim(proc)
        self.assertTrue(enclosing.exists(), 'the live output is inside this target')
        self.assertNotIn(enclosing, removed)
        self.assertIn(other, removed)

    def test_review_own_output_does_not_excuse_another_target_alias(self):
        own = self.target('own-owner')
        other = self.target('other-owner')
        alias = self.cache / 'target' / 'alias with space'
        alias.symlink_to(other, target_is_directory=True)
        proc = self.build(['HOME=' + str(self.home), 'CARGO_TARGET_DIR=' + str(own),
                           'CUSTOM_PATH=' + str(alias / 'debug/artifact')])
        self.assert_refused_on_merits(proc)
        self.assertTrue(own.exists())
        self.assertTrue(other.exists())

    def test_foreign_default_target_does_not_block_real_idle_target_eviction(self):
        target = self.target()
        proc = self.process()
        before = {p.relative_to(self.cwd): p.read_bytes()
                  for p in self.cwd.rglob('*') if p.is_file()}
        self.assertEqual(self.reclaim(proc), [target])
        self.assertFalse(target.exists())
        self.assertEqual((self.root / 'owner/source.rs').read_text(), 'keep')
        after = {p.relative_to(self.cwd): p.read_bytes()
                 for p in self.cwd.rglob('*') if p.is_file()}
        self.assertEqual(after, before, 'foreign manifests and lockfiles must not change')

    def test_foreign_target_inside_managed_root_still_refuses_all_eviction(self):
        target = self.target()
        proc = self.process(self.cache / 'target/unattributed')
        self.assert_refused_on_merits(proc)
        self.assertTrue(target.exists())

    def test_unreadable_foreign_environ_still_refuses_all_eviction(self):
        target = self.target()
        proc = self.process()
        # The census reads environ through cargo_observation.read, so the
        # unreadability must be injected on that path. Patching Path.open never
        # reached it, which is why this test used to pass without exercising it.
        actual = cargo_observation.read
        def unreadable(path, deadline):
            if Path(path).name == 'environ' and proc in Path(path).parents:
                raise PermissionError('fixture environ unreadable')
            return actual(path, deadline)
        with patch.object(cargo_observation, 'read', side_effect=unreadable):
            evidence = self.assert_refused_on_merits(proc)
        self.assertTrue(any(kind == 'unreadable' and surface == 'environ'
                            for kind, pid, surface, _ in evidence),
                        'the refusal must name the unreadable environ')
        self.assertTrue(target.exists())

    def test_unreadable_descendant_blocks_real_eviction(self):
        target = self.target()
        proc = self.process()
        read = cargo_observation.read
        def unreadable(path, deadline):
            if path.parent.name == self.descendant and path.name == 'environ':
                raise PermissionError('fixture descendant environ unreadable')
            return read(path, deadline)
        with patch.object(cargo_observation, 'read', side_effect=unreadable):
            self.assert_refused_on_merits(proc)
        self.assertTrue(target.exists())

    def test_default_uses_workspace_root_from_nested_member_without_writing_lock(self):
        (self.cwd / 'Cargo.toml').write_text('[workspace]\nmembers=["member"]\nresolver="2"\n')
        member = self.cwd / 'member'
        (member / 'src').mkdir(parents=True)
        (member / 'Cargo.toml').write_text('[package]\nname="member"\nversion="0.1.0"\n')
        (member / 'src/lib.rs').touch()
        self.assertEqual(self.resolve(cwd=member), {self.cwd / 'target'})
        self.assertFalse((self.cwd / 'Cargo.lock').exists())

    def test_explicit_manifest_path_selects_the_actual_workspace(self):
        other = self.root / 'other'
        other.mkdir()
        self.assertEqual(self.resolve(['--manifest-path', str(self.cwd / 'Cargo.toml')], cwd=other),
                         {self.cwd / 'target'})

    def test_cli_and_environment_precedence_and_relative_paths(self):
        self.config(self.cwd / '.cargo', '[build]\ntarget-dir="from-config"\n')
        env = {**self.environment, b'CARGO_BUILD_TARGET_DIR': b'from-build-env'}
        self.assertEqual(self.resolve(environment=env), {self.cwd / 'from-build-env'})
        env[b'CARGO_TARGET_DIR'] = b'from-target-env'
        self.assertEqual(self.resolve(environment=env), {self.cwd / 'from-target-env'})
        self.assertEqual(self.resolve(['--target-dir=from-cli'], env), {self.cwd / 'from-cli'})

    def test_foreign_builtin_with_non_path_config_overrides_can_be_attributed(self):
        arguments = ['cargo', '--config', 'build.rustc-wrapper=""',
                     '--config=build.rustc-workspace-wrapper=""', '--config',
                     'profile.distribution.panic="unwind"', 'build', '--target-dir',
                     str(self.cwd / 'separate-target')]
        self.assertTrue(cargo_targets.foreign(self.cwd, self.environment, arguments, {},
                                              time.monotonic() + 10))
        for override in ['other.toml', 'build.target-dir="elsewhere"',
                         'build.rustc-wrapper="custom"', 'profile.release.incremental=true']:
            with self.subTest(override=override), self.assertRaises(ValueError):
                cargo_targets.resolve(self.cwd, self.environment,
                                      ['cargo', '--config', override, 'build'],
                                      time.monotonic() + 10)

    def test_nearest_config_overrides_ancestors_and_home_with_config_relative_base(self):
        self.config(self.home / '.cargo', '[build]\ntarget-dir="home-output"\n')
        self.assertEqual(self.resolve(), {self.home / 'home-output'})
        self.config(self.root / '.cargo', '[build]\ntarget-dir="ancestor-output"\n')
        self.assertEqual(self.resolve(), {self.root / 'ancestor-output'})
        self.config(self.cwd / '.cargo', '[build]\ntarget-dir="local-output"\n')
        self.assertEqual(self.resolve(), {self.cwd / 'local-output'})
        self.config(self.cwd / '.cargo', '[build]\ntarget-dir="legacy-config"\n', 'config')
        self.assertEqual(self.resolve(), {self.cwd / 'legacy-config'})

    def test_cargo_home_override_and_managed_build_directory_are_not_ignored(self):
        custom = self.root / 'custom-cargo-home'
        self.config(custom, '[build]\ntarget-dir="custom-output"\n')
        env = {**self.environment, b'CARGO_HOME': os.fsencode(custom)}
        self.assertEqual(self.resolve(environment=env), {self.root / 'custom-output'})
        self.config(self.cwd / '.cargo', f'[build]\nbuild-dir="{self.cache}/target/build"\n')
        with patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}):
            self.assertFalse(cargo_targets.foreign(self.cwd, env, [b'cargo', b'check'], {}, time.monotonic() + 10))

    def test_symlink_alias_into_a_managed_target_is_not_foreign(self):
        target = self.target()
        alias = self.root / 'alias'
        alias.symlink_to(target, target_is_directory=True)
        env = {**self.environment, b'CARGO_TARGET_DIR': os.fsencode(alias)}
        self.assertFalse(cargo_targets.foreign(self.cwd, env, [b'cargo', b'check'], {target: self.root / 'owner'},
                                              time.monotonic() + 10))

    def test_config_alias_cannot_hide_an_active_managed_target(self):
        target = self.target()
        self.config(self.cwd / '.cargo',
                    f'[alias]\nprobecheck=["check","--target-dir","{target}"]\n')
        proc = self.process(command='probecheck')
        self.assert_refused_on_merits(proc)
        self.assertTrue(target.exists())

    def test_environment_alias_cannot_hide_an_active_managed_target(self):
        target = self.target()
        proc = self.process(command='probecheck',
                            extra_env={'CARGO_ALIAS_PROBECHECK': f'check --target-dir={target}'})
        self.assert_refused_on_merits(proc)
        self.assertTrue(target.exists())

    def test_incremental_codegen_paths_are_retained_across_flag_spellings(self):
        target = self.target()
        for option in [[b'-C', os.fsencode(f'incremental={target}')],
                       [os.fsencode(f'-Cincremental={target}')],
                       [os.fsencode(f'-C=incremental={target}')],
                       [b'--codegen', os.fsencode(f'incremental={target}')],
                       [os.fsencode(f'--codegen=incremental={target}')]]:
            with self.subTest(option=option):
                self.assertFalse(cargo_targets.foreign(
                    self.cwd, self.environment, [b'--out-dir', b'outside', *option],
                    {target: self.root / 'owner'}, time.monotonic() + 10, 'rustc'))
                outside = [a.replace(os.fsencode(target), os.fsencode(self.cwd / 'incremental'))
                           for a in option]
                self.assertTrue(cargo_targets.foreign(
                    self.cwd, self.environment, [b'--out-dir', b'outside', *outside],
                    {target: self.root / 'owner'}, time.monotonic() + 10, 'rustc'))

    def test_compiler_response_file_cannot_hide_active_managed_output(self):
        target = self.target()
        response = self.cwd / 'compiler.args'
        response.write_text(f'-C\nincremental={target}\n')
        args = [b'--out-dir', b'outside', os.fsencode('@' + str(response))]
        for kind in ('rustc', 'rustdoc'):
            with self.subTest(kind=kind):
                with self.assertRaises(ValueError):
                    cargo_targets.foreign(self.cwd, self.environment, args,
                                          {target: self.root / 'owner'},
                                          time.monotonic() + 10, kind)
        self.assertTrue(target.exists())

    def test_foreign_compilers_use_actual_outputs_and_keep_codegen_options(self):
        for kind in ('rustc', 'rustdoc'):
            with self.subTest(kind=kind), patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}):
                arguments = [b'--out-dir', os.fsencode(self.cwd / 'target'), b'-Cmetadata=fixture']
                self.assertTrue(cargo_targets.foreign(self.cwd, self.environment, arguments, {},
                                                      time.monotonic() + 10, kind))
                arguments[1] = os.fsencode(self.cache / 'target/private')
                self.assertFalse(cargo_targets.foreign(self.cwd, self.environment, arguments, {},
                                                       time.monotonic() + 10, kind))
        with self.assertRaises(ValueError):
            cargo_targets.resolve(self.cwd, self.environment,
                                  [b'--out-dir', b'outside', b'--emit=link=unresolved'],
                                  time.monotonic() + 10, 'rustc')

    def test_unreadable_or_unsupported_config_never_becomes_no_config(self):
        self.config(self.cwd / '.cargo', '[build]\ntarget-dir="outside"\n')
        with patch.object(cargo_targets, 'document', side_effect=PermissionError('unreadable')):
            with self.assertRaises(PermissionError):
                self.resolve()
        for text in ['[build', 'include=["other.toml"]\n', '[build]\ntarget-dir=1\n']:
            self.config(self.cwd / '.cargo', text)
            with self.assertRaises(ValueError):
                self.resolve()
        with self.assertRaises(ValueError):
            self.resolve(['--config', 'build.target-dir="elsewhere"'])

    def test_empty_or_oversized_paths_and_config_refuse(self):
        with self.assertRaises(ValueError):
            self.resolve(environment={**self.environment, b'CARGO_TARGET_DIR': b''})
        self.config(self.cwd / '.cargo', '#' * (cargo_targets.MAX_CONFIG_BYTES + 1))
        with self.assertRaises(ValueError):
            self.resolve()
