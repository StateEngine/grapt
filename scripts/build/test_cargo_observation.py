"""Whole-tree observation blocks managed references and missing evidence."""
import inspect
import os
from pathlib import Path
import shutil
import tempfile
import time
import unittest
from unittest.mock import patch

import cargo_observation as observation
import evict_targets


class ObservationTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.proc = self.root / 'proc'
        self.proc.mkdir()
        self.cwd = self.root / 'foreign'
        self.cwd.mkdir()
        self.managed = self.root / 'managed/target'
        self.managed.mkdir(parents=True)
        self.parent = self.process('11')

    def process(self, pid, children=()):
        path = self.proc / pid
        (path / 'fd').mkdir(parents=True)
        (path / 'task' / pid).mkdir(parents=True)
        (path / 'stat').write_bytes(pid.encode() + b' (cargo) R ' + b'0 ' * 18 + b'1\n')
        (path / 'task' / pid / 'children').write_text(' '.join(children))
        (path / 'cwd').symlink_to(self.cwd, target_is_directory=True)
        (path / 'cmdline').write_bytes(b'cargo\0check\0')
        (path / 'environ').write_bytes(b'HOME=/foreign\0')
        return path

    def observe(self):
        return observation.foreign(self.parent, self.proc, {self.managed}, time.monotonic() + 5)

    def live_targets(self):
        (self.parent / 'comm').write_text('cargo\n')
        executable = self.parent / 'exe'
        if not executable.is_symlink():
            executable.symlink_to('/fixture/cargo')
        if b'--target-dir' not in (self.parent / 'cmdline').read_bytes():
            (self.parent / 'cmdline').write_bytes(
                b'cargo\0check\0--target-dir\0' + os.fsencode(self.cwd / 'target') + b'\0')
        with patch.object(evict_targets, 'queued_targets', return_value=set()):
            return evict_targets.live_targets({self.managed: self.root / 'owner'},
                                              self.proc, time.monotonic() + 5)

    def test_a_closed_descriptor_does_not_clear_a_live_process(self):
        vanished = self.parent / 'fd/4'
        held = self.parent / 'fd/5'
        vanished.symlink_to(self.cwd / 'closed')
        held.symlink_to(self.managed / 'artifact')
        readlink = observation.os.readlink
        entries = observation.entries
        def ordered(path, deadline):
            values = entries(path, deadline)
            return sorted(values) if path == self.parent / 'fd' else values
        def closes(path, *args, **kwargs):
            if Path(path) == vanished:
                vanished.unlink()
                raise FileNotFoundError('one descriptor closed; process and other fd live')
            return readlink(path, *args, **kwargs)
        with patch.object(observation, 'entries', side_effect=ordered), \
                patch.object(observation.os, 'readlink', side_effect=closes):
            self.assertIsNone(self.live_targets(), 'a held managed file must veto clearance')

    def test_missing_metadata_under_a_live_pid_remains_unknown(self):
        read = observation.read
        def missing(path, deadline):
            if path == self.parent / 'environ':
                raise FileNotFoundError('metadata unavailable while PID remains live')
            return read(path, deadline)
        with patch.object(observation, 'read', side_effect=missing):
            self.assertIsNone(self.live_targets(), 'missing evidence is not process exit')

    def test_pid_reuse_after_a_missing_response_file_does_not_clear_the_successor(self):
        (self.parent / 'cmdline').write_bytes(
            b'cargo\0check\0--target-dir\0' + os.fsencode(self.cwd / 'target') + b'\0@missing\0')
        identity = observation.identity
        calls = 0
        def reused(path, deadline):
            nonlocal calls
            state, started = identity(path, deadline)
            if path == self.parent:
                calls += 1
                if calls >= 2:
                    (self.parent / 'environ').write_bytes(
                        b'HELD=' + os.fsencode(self.managed) + b'\0')
                    return state, b'2'
            return state, started
        with patch.object(observation, 'identity', side_effect=reused):
            self.assertIsNone(self.live_targets(), 'the replacement PID needs a fresh observation')

    def test_a_confirmed_exited_descendant_does_not_veto_its_live_parent(self):
        (self.parent / 'task/11/children').write_text('12')
        child = self.process('12')
        (child / 'comm').write_text('wrapper\n')
        (child / 'exe').symlink_to('/fixture/wrapper')
        children = observation.children
        calls = 0
        def reaped(path, deadline):
            nonlocal calls
            if path == child:
                calls += 1
                if calls == 2:
                    shutil.rmtree(child)
                    (self.parent / 'task/11/children').write_text('')
            return children(path, deadline)
        with patch.object(observation, 'children', side_effect=reaped):
            self.assertEqual(self.live_targets(), {self.cwd / 'target'},
                             'a confirmed exit holds nothing')

    def test_review_path_alias_preserves_filename_delimiters(self):
        for suffix in (' with space', '=value', ',value', '(value)'):
            with self.subTest(suffix=suffix):
                alias = self.managed.with_name(self.managed.name + '-backup' + suffix)
                alias.symlink_to(self.managed, target_is_directory=True)
                (self.parent / 'environ').write_bytes(
                    b'CARGO_TARGET_DIR=' + os.fsencode(alias) + b'\0')
                self.assertFalse(self.observe(), 'an alias still holds the managed root')

    def test_review_unresolvable_symlink_cycle_is_not_clearance(self):
        first = self.managed.with_name(self.managed.name + '-cycle-first')
        second = self.managed.with_name(self.managed.name + '-cycle-second')
        first.symlink_to(second)
        second.symlink_to(first)
        (self.parent / 'environ').write_bytes(
            b'CARGO_TARGET_DIR=' + os.fsencode(first) + b'\0')
        self.assertFalse(self.observe(), 'a resolution cycle is unknown evidence')

    def test_review_parent_components_resolve_after_symlinks(self):
        (self.managed / 'inner').mkdir()
        alias = self.root / 'alias-parent'
        alias.symlink_to(self.managed / 'inner', target_is_directory=True)
        spelling = alias / '..'
        self.assertEqual(spelling.resolve(strict=True), self.managed)
        (self.parent / 'environ').write_bytes(
            b'HELD=' + os.fsencode(spelling) + b'\0')
        self.assertFalse(self.observe(), 'parent traversal still names the managed root')

    def test_review_environment_search_flags_hold_their_managed_path(self):
        for key in (b'RUSTFLAGS', b'CARGO_ENCODED_RUSTFLAGS'):
            with self.subTest(key=key):
                (self.parent / 'environ').write_bytes(
                    key + b'=-L' + os.fsencode(self.managed) + b'\0')
                self.assertFalse(self.observe(), 'a joined search flag still names the root')

    def test_review_nested_carrier_overrun_never_clears_the_process(self):
        (self.parent / 'environ').write_bytes(
            b'RUSTFLAGS=' + b'-L' * (1 << 12 + 1)
            + os.fsencode(self.managed) + b'\0')
        try:
            clear = self.observe()
        except ValueError:
            clear = False
        self.assertFalse(clear, 'incomplete path expansion cannot prove clearance')

    def test_complete_foreign_tree_without_managed_references_is_ignored(self):
        (self.parent / 'task/11/children').write_text('12')
        self.process('12')
        self.assertTrue(self.observe())

    def test_managed_path_in_any_observed_surface_blocks(self):
        for surface in ('cwd', 'fd', 'argv', 'environ', 'response'):
            with self.subTest(surface=surface):
                # Reset only the owned fixture's observations for this case.
                (self.parent / 'cwd').unlink()
                (self.parent / 'cwd').symlink_to(self.cwd, target_is_directory=True)
                descriptor = self.parent / 'fd/7'
                if descriptor.is_symlink():
                    descriptor.unlink()
                (self.parent / 'cmdline').write_bytes(b'rustc\0--out-dir=outside\0')
                (self.parent / 'environ').write_bytes(b'HOME=/foreign\0')
                if surface == 'cwd':
                    (self.parent / 'cwd').unlink()
                    (self.parent / 'cwd').symlink_to(self.managed, target_is_directory=True)
                elif surface == 'fd':
                    descriptor.symlink_to(self.managed / 'held (deleted)')
                elif surface == 'argv':
                    (self.parent / 'cmdline').write_bytes(b'rustc\0-C\0incremental=' +
                                                        os.fsencode(self.managed) + b'\0')
                elif surface == 'environ':
                    (self.parent / 'environ').write_bytes(b'UNRECOGNIZED_KEY=' +
                                                         os.fsencode(self.managed) + b'\0')
                else:
                    response = self.cwd / 'args'
                    response.write_text(f'-C\nincremental={self.managed}\n')
                    (self.parent / 'cmdline').write_bytes(b'rustc\0--out-dir=outside\0@args\0')
                self.assertFalse(self.observe())

    def test_managed_reference_in_descendant_blocks_even_if_parent_is_clear(self):
        (self.parent / 'task/11/children').write_text('12')
        child = self.process('12')
        (child / 'environ').write_bytes(b'CUSTOM=' + os.fsencode(self.managed) + b'\0')
        self.assertFalse(self.observe())

    def test_children_from_non_main_threads_are_observed(self):
        (self.parent / 'task/13').mkdir()
        (self.parent / 'task/13/children').write_text('12')
        child = self.process('12')
        (child / 'fd/5').symlink_to(self.managed / 'artifact')
        self.assertFalse(self.observe())

    def test_unreadable_descendant_or_descriptor_does_not_mean_no_reference(self):
        (self.parent / 'task/11/children').write_text('12')
        self.process('12')
        read = observation.read
        def unreadable(path, deadline):
            if path == self.proc / '12/environ':
                raise PermissionError('unreadable descendant')
            return read(path, deadline)
        with patch.object(observation, 'read', side_effect=unreadable):
            with self.assertRaises(PermissionError):
                self.observe()
        (self.parent / 'fd/4').write_text('not a readable proc fd link')
        with self.assertRaises(OSError):
            self.observe()

    def test_missing_oversized_and_non_regular_response_files_refuse(self):
        response = self.cwd / 'args'
        (self.parent / 'cmdline').write_bytes(b'rustc\0@args\0')
        with self.assertRaises(FileNotFoundError):
            self.observe()
        response.write_bytes(b'x' * (observation.MAX_BYTES + 1))
        with self.assertRaises(ValueError):
            self.observe()
        response.unlink()
        os.mkfifo(response)
        with self.assertRaises(ValueError):
            self.observe()

    def test_foreign_tree_that_keeps_launching_children_is_still_admitted(self):
        # A foreign build churns for its whole run. Tree-level change is not
        # evidence about our targets, so a child appearing after its parent was
        # read must not turn the observation into a refusal.
        children = observation.children
        calls = 0
        def changed(path, deadline):
            nonlocal calls
            result = children(path, deadline)
            if path == self.parent:
                calls += 1
                result.add(str(100 + calls))
            return result
        with patch.object(observation, 'children', side_effect=changed):
            self.assertTrue(self.observe())

    def test_a_churning_foreign_process_that_references_the_cache_still_vetoes(self):
        # Admitted churn never becomes clearance: a compiler that holds our cache
        # in its own environment still refuses, even while its tree launches and
        # reaps children around it.
        children = observation.children
        calls = 0
        def churning(path, deadline):
            nonlocal calls
            result = children(path, deadline)
            if path == self.parent:
                calls += 1
                result.add(str(100 + calls))
            return result
        (self.parent / 'environ').write_bytes(b'CARGO_TARGET_DIR=' +
                                              os.fsencode(self.managed) + b'\0')
        with patch.object(observation, 'children', side_effect=churning):
            self.assertFalse(self.observe())

    def test_live_process_that_keeps_changing_identity_is_an_unknown(self):
        # A PID whose start time keeps moving cannot be proven clear, so it stays
        # a refusal rather than becoming an admission.
        identity = observation.identity
        calls = 0
        def reused(path, deadline):
            nonlocal calls
            result = identity(path, deadline)
            if path == self.parent:
                calls += 1
                return result[0], str(calls).encode()
            return result
        with patch.object(observation, 'identity', side_effect=reused):
            with self.assertRaisesRegex(ValueError, 'changed while it was observed'):
                self.observe()

    def test_process_exiting_during_observation_drops_out_without_veto(self):
        # The churn that used to refuse every pass: a compiler reaped mid-read.
        process = observation.observe
        seen = []
        def vanishing(path, references, deadline, responses):
            if path.name == '12':
                seen.append(path.name)
                return None
            return process(path, references, deadline, responses)
        (self.parent / 'task/11/children').write_text('12')
        self.process('12')
        with patch.object(observation, 'observe', side_effect=vanishing):
            self.assertTrue(self.observe())
        self.assertEqual(seen, ['12'])


class AncestorRuleTests(unittest.TestCase):
    """A build's layout ancestors protect its own output and nothing else.

    Each case is a token in a process's environment and a declared own output,
    and the verdict is whether that token makes the process foreign.
    """

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.proc = self.root/'proc'; self.proc.mkdir()
        (self.root/'foreign').mkdir()
        self.managed = self.root/'cache/target'
        self.container = self.managed/'toolchain/worktrees'
        self.own = self.container/'own-build'
        self.sibling = self.container/'sibling-build'
        for path in (self.own, self.sibling):
            path.mkdir(parents=True)
        self.entry = self.proc/'77'
        (self.entry/'fd').mkdir(parents=True)
        (self.entry/'task'/'77').mkdir(parents=True)
        (self.entry/'task'/'77'/'children').write_text('')
        (self.entry/'stat').write_bytes(b'77 (cargo) R ' + b'0 '*18 + b'1\n')
        (self.entry/'cwd').symlink_to(self.root/'foreign', target_is_directory=True)
        (self.entry/'cmdline').write_bytes(b'cargo\0build\0')

    def verdict(self, tokens, own):
        (self.entry/'environ').write_bytes(b'\0'.join(tokens) + b'\0')
        options = {}
        if 'own_output' in inspect.signature(observation.foreign).parameters:
            options['own_output'] = own
        # The caller always names the managed root and every candidate target it
        # may reclaim; the container's siblings are the candidate leaves.
        return observation.foreign(self.entry, self.proc,
                                   {self.managed, self.own, self.sibling},
                                   time.monotonic()+5,
                                   excused=b'CARGO_TARGET_DIR=' + os.fsencode(own),
                                   **options)

    def test_own_output_is_excused(self):
        # Only the token that *is* the declared own output is exempt; a second
        # path beside it would be a separate claim and is covered below.
        self.assertTrue(self.verdict([b'CARGO_TARGET_DIR='+os.fsencode(self.own)], self.own))

    def test_a_second_reference_inside_own_output_still_vetoes(self):
        # Rule 5 keeps the veto honest: the exemption is the one token, not the
        # subtree. A different token naming a path under own output is still a
        # second reference and is not silently cleared.
        (self.own/'debug').mkdir()
        self.assertFalse(self.verdict([b'CARGO_TARGET_DIR='+os.fsencode(self.own),
                                       b'X='+os.fsencode(self.own/'debug')], self.own))

    def test_an_ancestor_above_the_container_is_layout_not_a_claim(self):
        # The toolchain root and the managed root are both above the worktrees
        # container. A build inherits them; they do not name a sibling.
        for ancestor in (self.container.parent, self.managed):
            with self.subTest(ancestor=ancestor):
                self.assertTrue(self.verdict([b'CARGO_TARGET_DIR='+os.fsencode(self.own),
                                              b'SECODE_TARGET_ROOT='+os.fsencode(ancestor)],
                                             self.own))

    def test_the_container_itself_still_vetoes(self):
        # It stands for any sibling under it, so it may not be excused.
        self.assertFalse(self.verdict([b'CARGO_TARGET_DIR='+os.fsencode(self.own),
                                       b'X='+os.fsencode(self.container)], self.own))

    def test_a_sibling_target_still_vetoes(self):
        self.assertFalse(self.verdict([b'CARGO_TARGET_DIR='+os.fsencode(self.own),
                                       b'X='+os.fsencode(self.sibling)], self.own))

    def test_an_ancestor_with_no_resolvable_own_output_vetoes(self):
        # Rule 3: an unknown child. With no own output there is nothing for the
        # ancestor to be an ancestor OF, so it keeps its veto.
        (self.entry/'environ').write_bytes(
            b'SECODE_TARGET_ROOT=' + os.fsencode(self.container.parent) + b'\0')
        self.assertFalse(observation.foreign(self.entry, self.proc, {self.managed},
                                             time.monotonic()+5))

    def test_the_symlinked_spelling_resolves_the_same(self):
        link = self.root/'cache-link'
        link.symlink_to(self.root/'cache', target_is_directory=True)
        alias = link/'target'/'toolchain'/'worktrees'/'own-build'
        self.assertTrue(self.verdict([b'CARGO_TARGET_DIR='+os.fsencode(self.own),
                                      b'SECODE_TARGET_ROOT='+os.fsencode(link/'target'/'toolchain')],
                                     self.own))
        self.assertTrue(self.verdict([b'CARGO_TARGET_DIR='+os.fsencode(alias)], self.own))

    def test_an_enclosing_own_output_does_not_excuse_a_sibling(self):
        # A build whose own output encloses the candidates is that layout's
        # writer, not a claim on every sibling under it. A symlinked spelling
        # carries none of the managed root's bytes, so only resolution sees it.
        (self.root/'alias').symlink_to(self.container, target_is_directory=True)
        (self.root/'to-sibling').symlink_to(self.sibling, target_is_directory=True)
        for own, token in ((self.managed, self.root/'alias'/'sibling-build'),
                           (self.managed, self.root/'to-sibling'),
                           (self.container, self.root/'alias'/'sibling-build')):
            with self.subTest(own=own, token=token):
                self.assertFalse(self.verdict([b'CARGO_TARGET_DIR='+os.fsencode(own),
                                               b'X='+os.fsencode(token)], own))

    def test_a_nested_own_output_keeps_the_container_of_its_target(self):
        # A trybuild-style output below its target still lives beside the same
        # siblings, so the target itself is the container and still vetoes.
        nested = self.own/'tests'/'trybuild'
        nested.mkdir(parents=True)
        self.assertFalse(self.verdict([b'CARGO_TARGET_DIR='+os.fsencode(nested),
                                       b'X='+os.fsencode(self.container)], nested))
        self.assertFalse(self.verdict([b'CARGO_TARGET_DIR='+os.fsencode(nested),
                                       b'X='+os.fsencode(self.sibling)], nested))

    def test_another_candidates_parent_is_not_layout(self):
        # Mixed depths: a directory that directly holds a candidate is that
        # candidate's container, so it names a sibling even when it is also an
        # ancestor of own output. Only layout ABOVE every container is excused.
        tree = self.managed/'toolchain'
        other = tree/'other-build'
        container = tree/'worktrees'
        own = container/'nested-own'
        for path in (other, own):
            path.mkdir(parents=True, exist_ok=True)
        (self.entry/'environ').write_bytes(
            b'CARGO_TARGET_DIR='+os.fsencode(own)+b'\0'
            + b'X='+os.fsencode(tree)+b'\0')
        self.assertFalse(observation.foreign(
            self.entry, self.proc, {self.managed, container, other, own},
            time.monotonic()+5, excused=b'CARGO_TARGET_DIR='+os.fsencode(own),
            own_output=own))

    def test_a_census_over_many_candidates_stays_cheap(self):
        # A census walks cwd, argv, environ and every descriptor, so a lookup
        # that scans all roots per piece grows with candidates x descriptors.
        # One pass over each piece's own ancestry keeps it flat.
        targets = self.managed/'toolchain/worktrees'
        for index in range(128):
            (targets/f'{index:064x}').mkdir()
        for index in range(32):
            (self.root/f'input-{index}').touch()
            (self.entry/'fd'/str(index)).symlink_to(self.root/f'input-{index}')
        roots = {self.managed, *targets.iterdir()}
        (self.entry/'environ').write_bytes(
            b'CARGO_TARGET_DIR='+os.fsencode(self.own)+b'\0')
        start = time.process_time()
        self.assertTrue(observation.foreign(
            self.entry, self.proc, roots, time.monotonic()+120,
            excused=b'CARGO_TARGET_DIR='+os.fsencode(self.own),
            own_output=self.own))
        self.assertLess(time.process_time() - start, 0.5)
