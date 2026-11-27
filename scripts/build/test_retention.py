"""Source removal requires an independently restorable bundle."""
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
import stat
from threading import Event, Thread
from unittest.mock import patch

import evict_targets as targets
import orphan_cache
import retention


class FakeClock:
    """A controllable monotonic clock for budget tests."""

    def __init__(self):
        self.now = 1000000.0
        self._wall = time.time()

    def monotonic(self):
        return self.now

    def time(self):
        return self._wall

    def advance(self, seconds):
        self.now += seconds


class RetentionTests(unittest.TestCase):
    def test_archive_failure_preserves_source_and_success_restores_unlanded_head(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            main = root / 'main'
            subprocess.run(['git', 'init', '-q', str(main)], check=True, timeout=10)
            for name, text in [('Cargo.toml', '[workspace]\nmembers=[]\n'),
                               ('crates/grapt-types/Cargo.toml', '[package]\nname="grapt-types"\n'),
                               ('source', 'baseline\n')]:
                path = main / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            subprocess.run(['git', '-C', str(main), 'add', '--', 'Cargo.toml', 'crates', 'source'], check=True)
            def commit(path, files):
                subprocess.run(['git', '-C', str(path), 'add', '--', *files], check=True, timeout=10)
                subprocess.run(['git', '-C', str(path), '-c', 'user.name=Fixture Author',
                                '-c', 'user.email=fixture@users.noreply.github.com',
                                'commit', '-q', '-F', '-', '--', *files],
                               input=(b'chore(fixture): fixture state\n\n'
                                      b'Co-Authored-By: Codex <noreply@openai.com>\n'),
                               check=True, timeout=10)
            commit(main, ['Cargo.toml', 'crates/grapt-types/Cargo.toml', 'source'])
            lane = root / 'lane'
            retention.git(main, 'worktree', 'add', '-b', 'unlanded', str(lane))
            (lane / 'source').write_text('unlanded work\n')
            commit(lane, ['source'])
            expected = retention.git(lane, 'rev-parse', 'HEAD')
            now = time.time() + retention.RETENTION_SECONDS + 60
            deadline = time.monotonic() + 60
            archives = root / 'archives'
            proc = root / 'proc'
            proc.mkdir()
            real_git = retention.git
            def refusing(path, *arguments):
                if arguments[:2] == ('bundle', 'verify'):
                    raise RuntimeError('archive verification failed')
                return real_git(path, *arguments)
            with patch.object(retention, 'unused',
                                  side_effect=lambda paths, deadline, proc: set(paths)):
                with patch.object(retention, 'git', side_effect=refusing):
                    with self.assertRaisesRegex(RuntimeError, 'archive verification failed'):
                        retention.archive_worktree(lane, archives, now, deadline, apply=True, proc=proc)
                self.assertEqual((lane / 'source').read_text(), 'unlanded work\n')
                def truncated(path, *arguments):
                    value = real_git(path, *arguments)
                    if arguments[:2] == ('bundle', 'create'):
                        bundle = Path(arguments[2])
                        bundle.write_bytes(bundle.read_bytes()[:-16])
                    return value
                with patch.object(retention, 'git', side_effect=truncated):
                    with self.assertRaises(subprocess.CalledProcessError):
                        retention.archive_worktree(lane, archives, now, deadline, apply=True, proc=proc)
                self.assertTrue(lane.exists())
                real_sync = os.fsync
                def publication_fails(descriptor):
                    if stat.S_ISDIR(os.fstat(descriptor).st_mode) and list(archives.glob('*.bundle')):
                        raise OSError('durable publication failed')
                    return real_sync(descriptor)
                with patch.object(retention.os, 'fsync', side_effect=publication_fails):
                    with self.assertRaisesRegex(OSError, 'durable publication failed'):
                        retention.archive_worktree(lane, archives, now, deadline, apply=True, proc=proc)
                self.assertTrue(lane.exists())
                for incomplete in archives.glob('*.bundle'):
                    incomplete.unlink()
                retention.archive_worktree(lane, archives, now, deadline, apply=True, proc=proc)
            self.assertFalse(lane.exists())
            bundle, = archives.glob('*.bundle')
            restored = root / 'restored'
            subprocess.run(['git', 'clone', '-q', str(bundle), str(restored)], check=True, timeout=15)
            self.assertEqual(retention.git(restored, 'rev-parse', 'unlanded'), expected)
            self.assertEqual(retention.git(restored, 'show', expected + ':source'), 'unlanded work')

    def test_retention_preserves_recent_pinned_and_git_content(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ('expired', 'recent', 'pinned', 'repository'):
                path = root / name
                path.mkdir()
                (path / 'data').write_text('diagnostic\n')
            (root / 'pinned/.secode-retain').touch()
            (root / 'repository/.git').mkdir()
            old = time.time() - retention.RETENTION_SECONDS - 60
            for path in sorted(root.rglob('*'), reverse=True):
                if 'recent' not in path.parts:
                    os.utime(path, (old, old))
            proc = root / 'proc'
            proc.mkdir()
            with patch.object(retention, 'unused',
                                  side_effect=lambda paths, deadline, proc: set(paths)):
                retention.reclaim_scratch(time.time(), time.monotonic() + 30, True, (root,),
                                          proc=proc)
            self.assertFalse((root / 'expired').exists())
            for name in ('recent', 'pinned', 'repository'):
                self.assertTrue((root / name).exists())

    def test_root_mount_and_same_device_nested_bind_mount_are_preserved(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            candidate = root / 'expired'
            child = candidate / 'bound'
            child.mkdir(parents=True)
            (child / 'data').write_text('must remain\n')
            old = time.time() - retention.RETENTION_SECONDS - 60
            for path in (child / 'data', child, candidate):
                os.utime(path, (old, old))
            real_mounts = retention.mounts
            table = root / 'mountinfo'
            for mounted in (candidate, child):
                # Both rows deliberately have the same device number as their parent.
                table.write_text(f'1 0 8:1 / / rw - ext4 /dev/root rw\n'
                                 f'2 1 8:1 /original {mounted} rw - ext4 /dev/root rw\n')
                with patch.object(retention, 'mounts', side_effect=lambda: real_mounts(table)):
                    with patch.object(retention, 'unused',
                                  side_effect=lambda paths, deadline, proc: set(paths)):
                        retention.reclaim_scratch(time.time(), time.monotonic() + 30, True, (root,),
                                                  proc=root / 'proc')
                self.assertEqual((child / 'data').read_text(), 'must remain\n')

    def test_backlog_larger_than_one_budget_drains_over_runs(self):
        """A backlog that outgrows one 900 s budget drains over several runs,
        each exiting 0 with an honest marker, instead of failing the run."""
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / 'scratch'
            root.mkdir()
            total = 150
            for index in range(total):
                tree = root / f'alpha-tree-{index:03d}'
                tree.mkdir()
                (tree / 'log').write_text('x')
            old = time.time() - retention.RETENTION_SECONDS - 60
            for path in sorted(root.rglob('*'), reverse=True):
                os.utime(path, (old, old))
            data = base / 'data'
            data.mkdir()
            clock = FakeClock()
            def priced(paths, deadline, proc):
                # A full-list census costs 65 budget seconds; the fresh
                # single-path removal gate costs ~0.1 s, modeled as free.
                if len(paths) > 1:
                    clock.advance(65)
                return set(paths)
            def slow_idle(path, now, deadline, **kwargs):
                clock.advance(4)
                return True
            def run():
                with patch.object(retention, 'time', clock), \
                        patch.object(targets, 'time', clock), \
                        patch.object(retention, 'unused', side_effect=priced), \
                        patch.object(retention, 'idle', side_effect=slow_idle), \
                        patch.object(retention, 'ROOTS', (root,)), \
                        patch.object(retention, 'data_root', return_value=data), \
                        patch.object(sys, 'argv', ['retention.py', '--apply']), \
                        contextlib.redirect_stdout(io.StringIO()) as out:
                    retention.main()
                return out.getvalue()
            first = run()
            removed_first = total - len(list(root.glob('alpha-tree-*')))
            self.assertIn('backlog remains:', first)
            self.assertIn(f'removed {removed_first} this run', first)
            self.assertGreater(removed_first, 0, 'a progress run must remove trees')
            self.assertLess(removed_first, total, 'the backlog outgrows one budget')
            for _ in range(8):
                if not list(root.glob('alpha-tree-*')):
                    break
                output = run()
                if list(root.glob('alpha-tree-*')):
                    self.assertIn('backlog remains:', output)
                else:
                    break
            self.assertEqual(len(list(root.glob('alpha-tree-*'))), 0)
            progress = json.loads((data / 'state' / 'retention-progress.json').read_text())
            self.assertEqual(progress['zero_progress_runs'], 0)
            self.assertIsInstance(progress['last_disk_free_bytes'], int)

    def test_zero_progress_streak_escalates_after_three_runs(self):
        """K consecutive runs that remove nothing exit non-zero; progress
        resets the streak."""
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / 'scratch'
            root.mkdir()
            for name in ('alpha-held', 'beta-held'):
                tree = root / name
                tree.mkdir()
                (tree / 'log').write_text('x')
            old = time.time() - retention.RETENTION_SECONDS - 60
            for path in sorted(root.rglob('*'), reverse=True):
                os.utime(path, (old, old))
            data = base / 'data'
            data.mkdir()
            def held(paths, deadline, proc):
                return set()  # every tree is held by a live process
            def run(free=False):
                unused = (lambda paths, deadline, proc: set(paths)) if free else held
                with patch.object(retention, 'unused', side_effect=unused), \
                        patch.object(retention, 'ROOTS', (root,)), \
                        patch.object(retention, 'data_root', return_value=data), \
                        patch.object(sys, 'argv', ['retention.py', '--apply']), \
                        contextlib.redirect_stdout(io.StringIO()) as out:
                    retention.main()
                return out.getvalue()
            def streak():
                return json.loads(
                    (data / 'state' / 'retention-progress.json').read_text())['zero_progress_runs']
            first = run()
            self.assertIn('backlog remains: 2 trees', first)
            self.assertIn('removed 0 this run', first)
            self.assertEqual(streak(), 1)
            self.assertIn('backlog remains: 2 trees', run())
            self.assertEqual(streak(), 2)
            with self.assertRaises(SystemExit) as raised:
                run()
            self.assertEqual(raised.exception.code, 2)
            self.assertEqual(streak(), 3)
            for name in ('alpha-held', 'beta-held'):
                self.assertTrue((root / name).exists())
            self.assertNotIn('backlog remains:', run(free=True))
            self.assertEqual(streak(), 0)
            for name in ('alpha-held', 'beta-held'):
                self.assertFalse((root / name).exists())

    def test_census_reuse_spares_a_tree_a_live_process_holds(self):
        """The cached census filters the whole list, and the fresh single-path
        census that gates the removal still spares the tree a live process
        holds (the #319 synthetic process table)."""
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            root = base / 'scratch'
            root.mkdir()
            held = root / 'alpha-held'
            free = root / 'alpha-free'
            held.mkdir()
            free.mkdir()
            (held / 'log').write_text('x')
            (free / 'log').write_text('x')
            old = time.time() - retention.RETENTION_SECONDS - 60
            for path in sorted(root.rglob('*'), reverse=True):
                os.utime(path, (old, old))
            process = base / 'proc' / '123'
            (process / 'fd').mkdir(parents=True)
            (process / 'stat').write_text('123 (fixture) S 1 1')
            (process / 'comm').write_bytes(b'python\n')
            (process / 'cmdline').write_bytes(b'/usr/bin/python\0work\0')
            (process / 'environ').write_bytes(b'HOME=/fixture\0')
            (process / 'exe').symlink_to(sys.executable)
            (process / 'cwd').symlink_to(held)
            calls = []
            scanner = orphan_cache.active
            def counting(paths, deadline, proc):
                calls.append(len(paths))
                return scanner(paths, deadline, proc)
            with patch.object(orphan_cache, 'active', side_effect=counting):
                removed, remaining, exhausted, scan_complete = retention.reclaim_scratch(
                    time.time(), time.monotonic() + 60, True, (root,), proc=base / 'proc')
            self.assertEqual(removed, [free])
            self.assertTrue(held.exists())
            self.assertFalse(free.exists())
            self.assertEqual((exhausted, scan_complete), (False, True))
            self.assertEqual(remaining, [held])
            self.assertEqual(calls, [2, 1],
                             'one full census must filter; a fresh single-path census must gate the removal')

    def test_late_holder_spared_by_fresh_single_path_census(self):
        """A process that takes a reference to the second tree after the first
        removal must be seen by the fresh single-path census that gates that
        removal; the cached filter alone would rm the tree it now holds."""
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            scratch = base / 'scratch'
            scratch.mkdir()
            proc = base / 'proc'
            proc.mkdir()
            first = scratch / 'a-first'
            second = scratch / 'b-second'
            for tree in (first, second):
                tree.mkdir()
                (tree / 'fixture').write_text('fixture')
            old = time.time() - retention.RETENTION_SECONDS - 60
            for path in sorted(scratch.rglob('*'), reverse=True):
                os.utime(path, (old, old))
            def hold(source, tree):
                process = source / '1001'
                (process / 'fd').mkdir(parents=True)
                (process / 'stat').write_text('1001 (fixture) S 1 1')
                (process / 'comm').write_bytes(b'fixture\n')
                (process / 'cmdline').write_bytes(b'fixture\0')
                (process / 'environ').write_bytes(b'HOME=/fixture\0')
                (process / 'exe').symlink_to(sys.executable)
                (process / 'cwd').symlink_to(tree)
            clock = FakeClock()
            start = clock.now
            censuses = []
            scanner = orphan_cache.active
            def observed(paths, deadline, source):
                result = scanner(paths, deadline, source)
                censuses.append((clock.monotonic(), [str(item) for item in sorted(paths)]))
                return result
            removals = []
            real_run = retention.subprocess.run
            def removal(argv, **kwargs):
                chosen = Path(argv[-1])
                removals.append((chosen.name, clock.monotonic()))
                if chosen == first:
                    clock.advance(10)
                    hold(proc, second)
                return real_run(argv, **kwargs)
            with patch.object(retention, 'time', clock), \
                    patch.object(targets, 'time', clock), \
                    patch.object(orphan_cache, 'active', side_effect=observed), \
                    patch.object(retention.subprocess, 'run', side_effect=removal):
                removed, remaining, exhausted, scan_complete = retention.reclaim_scratch(
                    time.time() + retention.RETENTION_SECONDS + 60, clock.monotonic() + 900,
                    True, (scratch,), proc=proc)
            self.assertEqual(removed, [first])
            self.assertTrue(second.exists(), 'the late-held tree must be spared')
            self.assertFalse(first.exists())
            self.assertEqual(remaining, [second])
            self.assertEqual((exhausted, scan_complete), (False, True))
            self.assertEqual(removals, [('a-first', start)])
            self.assertEqual(len(censuses), 3)
            self.assertEqual(censuses[0], (start, [str(first), str(second)]))
            self.assertEqual(censuses[1], (start, [str(first)]))
            self.assertEqual(censuses[-1], (start + 10, [str(second)]),
                             'every removal must be gated by a fresh single-path census')

    def test_blind_census_at_deadline_refuses(self):
        """A blind observation that reaches the handler after the deadline is
        a refusal, not budget exhaustion: it must raise, so main exits 1."""
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            scratch = base / 'scratch'
            scratch.mkdir()
            proc = base / 'proc'
            proc.mkdir()
            tree = scratch / 'expired'
            tree.mkdir()
            (tree / 'fixture').write_text('fixture')
            old = time.time() - retention.RETENTION_SECONDS - 60
            os.utime(tree / 'fixture', (old, old))
            os.utime(tree, (old, old))
            clock = FakeClock()
            def blind(paths, deadline, source):
                clock.advance(deadline - clock.monotonic())
                return None
            with patch.object(retention, 'time', clock), \
                    patch.object(targets, 'time', clock), \
                    patch.object(orphan_cache, 'active', side_effect=blind), \
                    patch.object(retention.subprocess, 'run',
                                 side_effect=AssertionError('unexpected deletion')):
                with self.assertRaises(retention.CensusUnavailable) as raised:
                    retention.reclaim_scratch(time.time() + retention.RETENTION_SECONDS + 60,
                                              clock.monotonic() + 900, True, (scratch,), proc=proc)
            self.assertIn('Process activity is unavailable', str(raised.exception))

    def test_paced_backlog_removes_every_tree_without_scan_sizing(self):
        """The scan no longer sizes candidates: a paced 4-tree fixture removes
        all four under the budget, and the only size walks are the per-removal
        ones, each immediately before its own rm."""
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            scratch = base / 'scratch'
            scratch.mkdir()
            proc = base / 'proc'
            proc.mkdir()
            for index in range(4):
                tree = scratch / f'tree-{index}'
                tree.mkdir()
                (tree / 'fixture').write_text('fixture')
            old = time.time() - retention.RETENTION_SECONDS - 60
            for path in sorted(scratch.rglob('*'), reverse=True):
                os.utime(path, (old, old))
            clock = FakeClock()
            start = clock.now
            events = []
            sized = retention.tree_bytes
            def counted_size(path, deadline):
                events.append(('size', path.name, clock.monotonic()))
                return sized(path, deadline)
            def paced_idle(path, now, deadline, **kwargs):
                clock.advance(0.5)
                return True
            real_run = retention.subprocess.run
            def removal(argv, **kwargs):
                events.append(('rm', Path(argv[-1]).name, clock.monotonic()))
                return real_run(argv, **kwargs)
            with patch.object(retention, 'time', clock), \
                    patch.object(targets, 'time', clock), \
                    patch.object(retention, 'idle', side_effect=paced_idle), \
                    patch.object(retention, 'tree_bytes', side_effect=counted_size), \
                    patch.object(retention, 'unused', side_effect=lambda paths, d, p: set(paths)), \
                    patch.object(retention.subprocess, 'run', side_effect=removal):
                removed, remaining, exhausted, scan_complete = retention.reclaim_scratch(
                    time.time() + retention.RETENTION_SECONDS + 60, clock.monotonic() + 5,
                    True, (scratch,), proc=proc)
            self.assertEqual(removed, [scratch / f'tree-{index}' for index in range(4)])
            self.assertEqual(remaining, [])
            self.assertEqual((exhausted, scan_complete), (False, True))
            self.assertLess(clock.now - start, 5.0, 'the paced backlog fits one budget')
            for index in range(0, len(events), 2):
                size_event, rm_event = events[index:index + 2]
                self.assertEqual(size_event[0], 'size')
                self.assertEqual(rm_event[0], 'rm')
                self.assertEqual(size_event[1], rm_event[1],
                                 'every size walk is the per-removal one, not a scan sizing')
                self.assertEqual(size_event[2], rm_event[2])

    def test_retain_marker_added_during_gate_census_spares_tree(self):
        """A .secode-retain marker created while the gate census runs must
        spare the tree: the fast marker re-check comes after the census, so
        the rm is never attempted."""
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            scratch = base / 'scratch'
            scratch.mkdir()
            proc = base / 'proc'
            proc.mkdir()
            tree = scratch / 'expired'
            tree.mkdir()
            (tree / 'fixture').write_text('fixture')
            old = time.time() - retention.RETENTION_SECONDS - 60
            for path in sorted(scratch.rglob('*'), reverse=True):
                os.utime(path, (old, old))
            seen = []
            scanner = orphan_cache.active
            def marking(paths, deadline, source):
                result = scanner(paths, deadline, source)
                seen.append(len(paths))
                if len(seen) == 2:
                    (tree / '.secode-retain').touch()
                return result
            with patch.object(orphan_cache, 'active', side_effect=marking), \
                    patch.object(retention.subprocess, 'run',
                                 side_effect=AssertionError('unexpected deletion')):
                removed, remaining, exhausted, scan_complete = retention.reclaim_scratch(
                    time.time(), time.monotonic() + 60, True, (scratch,), proc=proc)
            self.assertEqual(removed, [])
            self.assertEqual(remaining, [tree])
            self.assertEqual((exhausted, scan_complete), (False, True))
            self.assertEqual(seen, [1, 1],
                             'the gate census runs, then the marker re-check spares the tree')
            self.assertTrue((tree / '.secode-retain').exists())
            self.assertTrue(tree.exists())

    def test_archive_gate_census_is_fresher_than_the_idle_walk(self):
        """The archive's final gate pays its slow checks (the idle walk)
        before the fresh census, so git worktree remove never acts on an
        observation older than the idle walk that preceded it."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lane = root / 'lane'
            lane.mkdir()
            (lane / '.git').write_text('gitdir: fixture')
            gitdir = root / 'gitdir'
            gitdir.mkdir()
            common = root / 'common'
            common.mkdir()
            archives = root / 'archives'
            proc = root / 'proc'
            proc.mkdir()
            class Clock:
                def __init__(self):
                    self.elapsed = 0
                def monotonic(self):
                    return self.elapsed
                def advance(self, seconds):
                    self.elapsed += seconds
            clock = Clock()
            censuses = []
            idle_ends = []
            removals = []
            commit = 'a' * 40
            def git_mock(path, *words):
                if words[:2] == ('rev-parse', '--absolute-git-dir'):
                    return str(gitdir)
                if words[:2] == ('rev-parse', '--path-format=absolute'):
                    return str(common)
                if words[:2] == ('rev-parse', 'HEAD'):
                    return commit
                if words[:2] == ('worktree', 'list'):
                    return ''
                if words[:2] == ('bundle', 'create'):
                    Path(words[2]).write_text('synthetic bundle')
                if words[:2] == ('bundle', 'list-heads'):
                    return f'{commit} HEAD'
                if words[:2] == ('worktree', 'remove'):
                    removals.append(clock.monotonic())
                return ''
            def aging_idle(path, now, deadline, *, scratch=False):
                if len(idle_ends) == 1:
                    clock.advance(61)
                idle_ends.append(clock.monotonic())
                return True
            def scan(paths, deadline, source):
                censuses.append(clock.monotonic())
                return set(paths)
            with patch.object(retention, 'time', clock), \
                    patch.object(targets, 'time', clock), \
                    patch.object(retention, 'git', side_effect=git_mock), \
                    patch.object(retention, 'dirty', return_value=''), \
                    patch.object(retention.target_discovery, 'secode_checkout', return_value=True), \
                    patch.object(retention, 'idle', side_effect=aging_idle), \
                    patch.object(retention, 'unused', side_effect=scan), \
                    patch.object(retention.subprocess, 'run',
                                 return_value=subprocess.CompletedProcess([], 0)), \
                    contextlib.redirect_stdout(io.StringIO()):
                retention.archive_worktree(lane, archives, 800000, 200, True, proc=proc)
            self.assertEqual(idle_ends, [0, 61],
                             'the gate idle walk is the slow check that ages')
            self.assertEqual(censuses, [0, 61],
                             'the gate census must be taken after the slow idle walk')
            self.assertEqual(removals, [61])
            self.assertGreaterEqual(censuses[-1], idle_ends[-1],
                                    'the removal census must not be older than the idle walk')
            self.assertGreaterEqual(removals[0], censuses[-1])

    def test_retain_marker_during_blocked_stdout_never_meets_the_rm(self):
        """A full 64 KiB pipe blocks the stdout write. A .secode-retain
        marker created during that block must never meet the rm: the only
        stdout write around the removal is the outcome log, and it comes
        only after a successful removal."""
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            scratch = root / 'scratch'
            scratch.mkdir()
            tree = scratch / 'expired'
            tree.mkdir()
            os.utime(tree, (1, 1))
            reader, writer = os.pipe()
            os.set_blocking(writer, False)
            filled = 0
            try:
                while True:
                    filled += os.write(writer, b'x' * 4096)
            except BlockingIOError:
                pass
            self.assertGreater(filled, 0)
            os.set_blocking(writer, True)
            blocked = Event()
            attempts = []
            result = []
            errors = []

            def observe(argv, *, check, timeout):
                attempts.append({'path': Path(argv[-1]).name,
                                 'marker_at_removal': (tree / '.secode-retain').exists()})
                return subprocess.CompletedProcess(argv, 0)

            class PipeOutput:
                def write(self, message):
                    blocked.set()
                    return os.write(writer, message.encode())

                def flush(self):
                    pass

            def reclaim():
                try:
                    result.append(retention.reclaim_scratch(
                        10 ** 9, time.monotonic() + 10, True, (scratch,),
                        proc=root / 'fixture-proc'))
                except BaseException as error:
                    errors.append(repr(error))

            with patch.object(retention, 'mounts', return_value=set()), \
                    patch.object(retention, 'unused',
                                 side_effect=lambda paths, *_: set(paths)), \
                    patch.object(retention.subprocess, 'run', side_effect=observe), \
                    patch.object(sys, 'stdout', PipeOutput()):
                worker = Thread(target=reclaim, daemon=True)
                worker.start()
                self.assertTrue(blocked.wait(2) and worker.is_alive(),
                                'the outcome log must block on the full pipe')
                (tree / '.secode-retain').touch()
                self.assertEqual(len(os.read(reader, 4096)), 4096)
                worker.join(2)
                self.assertFalse(worker.is_alive())
                self.assertEqual(errors, [])
            os.close(writer)
            os.close(reader)
            removed, remaining, exhausted, scan_complete = result[0]
            self.assertEqual(removed, [tree])
            self.assertEqual(remaining, [])
            self.assertEqual((exhausted, scan_complete), (False, True))
            self.assertEqual([attempt['path'] for attempt in attempts], ['expired'])
            self.assertFalse(attempts[0]['marker_at_removal'],
                             'the rm ran while the retain marker was present')
