"""Delete real disposable fixtures while keeping source, active leases and pins."""
import hashlib
import contextlib
import fcntl
import io
import json
import os
from pathlib import Path
import subprocess
import shutil
import tempfile
import time
import sys
import unittest
from unittest.mock import patch

import disk_hygiene as hygiene
import evict_targets


class HygieneTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.cache = self.home/'cache'; self.cache.mkdir()
        # An empty process table: these tests stub both censuses, and the root
        # is passed explicitly so a path that forgets the stub fails loudly
        # instead of reading the host.
        self.proc = self.home/'proc'; self.proc.mkdir()
        self.now = time.time()
        # Unit cleanup owns only its fixture roots, including the RAM reaper.
        ram_root = patch.object(hygiene.ram_targets, 'ROOT', self.home/'ram')
        ram_root.start()
        self.addCleanup(ram_root.stop)
        self.references = hygiene.orphan_cache.active
        references = patch.object(hygiene.orphan_cache, 'active', return_value=set())
        references.start()
        self.addCleanup(references.stop)
        # A cleanup pass records what it decided. Keep that record inside this
        # fixture: a test must never write to the operator's logs.
        self.logs = self.home/'logs'
        journal = patch.object(hygiene.clean_journal, 'journal_path',
                               lambda directory=None: self.logs/'target-clean.json')
        journal.start()
        self.addCleanup(journal.stop)
        # The per-pass outcome record is a second record written by the same
        # path. It belongs in the fixture for the same reason.
        outcomes = patch.object(hygiene.hygiene_outcomes, 'record_path',
                                lambda directory=None: self.home/'hygiene-outcomes.json')
        outcomes.start()
        self.addCleanup(outcomes.stop)

    def target(self, name, age=7200):
        owner = self.home/name; owner.mkdir()
        (owner/'source.rs').write_text('keep source')
        key = hashlib.sha256(str(owner).encode()).hexdigest()
        target = self.cache/'target/toolchain/worktrees'/key
        (target/'debug').mkdir(parents=True)
        (target/'.secode-worktree').write_text(str(owner)+'\n')
        (target/'debug/artifact').write_text('compiled')
        self.age(target, age)
        return target, owner

    def age(self, root, age):
        for path in [*root.rglob('*'), root]:
            os.utime(path, (self.now-age, self.now-age), follow_symlinks=False)

    def reclaim(self, **kwargs):
        with patch.object(evict_targets, 'live_targets', return_value=set()):
            return hygiene.reclaim(self.cache, self.home, now=self.now,
                                   proc=self.proc, **kwargs)

    def test_idle_targets_removed_but_source_recent_targets_and_other_cache_survive(self):
        old, owner = self.target('old')
        other, _ = self.target('other-idle')
        oldest, later = sorted([old, other], key=str, reverse=True)
        self.age(oldest, 10800)
        recent, _ = self.target('recent', age=60)
        (self.cache/'sccache').mkdir(); (self.cache/'sccache/entry').write_text('keep')
        self.assertEqual(self.reclaim(), [oldest, later])
        self.assertFalse(old.exists()); self.assertFalse(other.exists()); self.assertTrue(recent.exists())
        self.assertEqual((owner/'source.rs').read_text(), 'keep source')
        self.assertEqual((self.cache/'sccache/entry').read_text(), 'keep')

    def test_ram_cleanup_failure_does_not_block_independent_disk_cleanup_or_report_success(self):
        old,owner=self.target('old')
        with patch.object(hygiene.ram_targets,'reap',side_effect=RuntimeError('owned scope still active')):
            with self.assertRaisesRegex(RuntimeError,'Disk cleanup completed, but RAM cleanup remains incomplete'):
                self.reclaim()
        self.assertFalse(old.exists())
        self.assertEqual((owner/'source.rs').read_text(),'keep source')

    def test_recent_nested_artifact_unknown_content_and_retention_pin_survive(self):
        recent, _ = self.target('nested')
        os.utime(recent/'debug/artifact', (self.now,self.now))
        pinned, _ = self.target('pinned'); (pinned/'.retain').write_text('keep')
        unknown, _ = self.target('unknown'); (unknown/'checkpoint').mkdir()
        self.assertEqual(self.reclaim(), [])
        self.assertTrue(all(p.exists() for p in [recent,pinned,unknown]))

    def test_old_gate_retention_tag_does_not_keep_idle_artifacts_forever(self):
        target, owner = self.target('old-qualified')
        (target/'.secode-retain').write_text(str(owner)+'\n')
        self.age(target,7200)
        self.assertEqual(self.reclaim(),[target])

    def test_checkpoint_fixture_scratch_is_derived_but_unknown_content_still_protects_target(self):
        target, owner = self.target('checkpoint-fixtures')
        scratch = target/'checkpoint-tests/interrupted-fixture'
        scratch.mkdir(parents=True)
        (scratch/'journal').write_text('generated checkpoint fixture')
        unknown, _ = self.target('unknown-fixtures')
        (unknown/'checkpoint-tests').mkdir()
        (unknown/'unrecognized').mkdir()
        self.age(target,7200)
        self.assertEqual(self.reclaim(),[target])
        self.assertTrue((owner/'source.rs').exists())
        self.assertTrue(unknown.exists())

    def test_large_factory_home_does_not_block_known_target_cleanup(self):
        target, _ = self.target('idle')
        for index in range(4100):
            (self.home/f'bookkeeping-{index}').touch()
        self.assertEqual(self.reclaim(),[target])

    def test_active_shared_lease_or_live_process_prevents_cleanup(self):
        target, _ = self.target('leased')
        with evict_targets.lease(target, shared=True):
            self.assertEqual(self.reclaim(), [])
        with patch.object(evict_targets,'live_targets',return_value={target}):
            self.assertEqual(hygiene.reclaim(self.cache,self.home,now=self.now,proc=self.proc),[])
        with patch.object(evict_targets,'live_targets',return_value=None):
            with self.assertRaisesRegex(RuntimeError,'Unknown live Cargo'):
                hygiene.reclaim(self.cache,self.home,now=self.now,proc=self.proc)
        self.assertTrue(target.exists())

    def test_a_refusal_names_the_process_and_the_reference_that_caused_it(self):
        # The contract: a refusal proves its cause. A synthetic census refuses
        # over one process, and the error must name the pid and what it did.
        process = self.proc/'4242'; process.mkdir()
        (process/'comm').write_bytes(b'cargo\n')
        (process/'stat').write_text('4242 (cargo) S ' + '0 '*18 + '1\n')
        (process/'task'/'4242').mkdir(parents=True)
        (process/'task'/'4242'/'children').write_text('')
        (process/'cmdline').write_bytes(b'cargo\0test\0')
        # The refusal this witness exercises is a token inside the MANAGED root,
        # which is read from SECODE_CACHE_DIR/target. A path under it is a real
        # managed reference whatever the owners map holds.
        target_root = self.cache/'target'
        (target_root/'toolchain').mkdir(parents=True, exist_ok=True)
        other = target_root/'toolchain/worktrees'/'a-sibling'
        other.mkdir(parents=True, exist_ok=True)
        # Its own output must EXIST, or the missing declaration becomes the cause.
        own = self.cache/'elsewhere'; own.mkdir()
        (process/'environ').write_bytes(
            b'CARGO_TARGET_DIR=' + os.fsencode(own) + b'\0'
            + b'X=' + os.fsencode(other) + b'\0')
        (process/'cwd').symlink_to(self.home, target_is_directory=True)
        (process/'exe').symlink_to('/usr/bin/cargo')
        (process/'fd').mkdir()
        (self.proc/'sys/kernel/random').mkdir(parents=True, exist_ok=True)
        (self.proc/'sys/kernel/random/boot_id').write_text('fixture-boot\n')
        # The managed root is SECODE_CACHE_DIR/target; keep it inside the fixture.
        cache_root = patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)})
        cache_root.start()
        self.addCleanup(cache_root.stop)
        with self.assertRaises(RuntimeError) as raised:
            hygiene.active_targets({}, time.monotonic() + 20, self.proc)
        message = str(raised.exception)
        self.assertIn('Unknown live Cargo', message)
        self.assertIn('4242', message, 'the refusal must name the process')
        self.assertIn('environment variable X', message,
                      'the refusal must name the source, never its value')

    def test_a_missing_declared_output_is_named_as_the_cause(self):
        # The cause is the missing declaration, not a foreign reference: zero
        # managed paths are named. The message must say so, or a reader looks in
        # the wrong place.
        process = self.proc/'61'; process.mkdir()
        (process/'task'/'61').mkdir(parents=True)
        (process/'task'/'61'/'children').write_text('')
        (process/'stat').write_bytes(b'61 (cargo) R ' + b'0 '*18 + b'1\n')
        (process/'comm').write_bytes(b'cargo\n')
        (process/'fd').mkdir()
        foreign = self.home/'foreign'; foreign.mkdir()
        (process/'cwd').symlink_to(foreign, target_is_directory=True)
        (process/'exe').symlink_to('/usr/bin/cargo')
        (process/'cmdline').write_bytes(b'cargo\0build\0')
        missing = self.home/'outside'/'no-such-output'
        (process/'environ').write_bytes(
            b'CARGO_TARGET_DIR=' + os.fsencode(missing) + b'\0')
        (self.proc/'sys/kernel/random').mkdir(parents=True, exist_ok=True)
        (self.proc/'sys/kernel/random/boot_id').write_text('fixture-boot\n')
        with self.assertRaises(RuntimeError) as raised:
            hygiene.active_targets({}, time.monotonic() + 20, self.proc)
        message = str(raised.exception)
        self.assertIn('does not exist', message)
        self.assertIn('environment variable CARGO_TARGET_DIR', message)
        self.assertNotIn('named a target reference', message)

    def test_an_all_hidden_unapproved_identity_is_named_as_the_cause(self):
        # Every gated source is hidden, so the non-dumpable signature IS present.
        # The refusal is the unapproved identity, and the message must not claim
        # the signature is absent.
        process = self.proc/'77'; process.mkdir()
        (process/'stat').write_text('77 (mystery) S 1 1')
        (process/'comm').write_bytes(b'mystery\n')
        (process/'cmdline').write_bytes(b'mystery\0')
        (process/'environ').write_bytes(b'HOME=/fixture\0')
        (process/'fd').mkdir()
        (process/'exe').symlink_to('/usr/bin/mystery')
        (process/'cwd').symlink_to(self.home)
        (self.proc/'sys/kernel/random').mkdir(parents=True, exist_ok=True)
        (self.proc/'sys/kernel/random/boot_id').write_text('fixture-boot\n')
        original_open, original_link = Path.open, os.readlink
        original_entries = hygiene.orphan_cache.targets.bounded_entries

        def hidden_open(path, *args, **kwargs):
            if Path(path).parent == process and Path(path).name == 'environ':
                raise PermissionError('fixture non-dumpable source')
            return original_open(path, *args, **kwargs)

        def hidden_link(path, *args, **kwargs):
            target = Path(path)
            if target.parent == process and target.name in ('exe', 'cwd'):
                raise PermissionError('fixture non-dumpable source')
            return original_link(path, *args, **kwargs)

        def hidden_entries(path, *args, **kwargs):
            target = Path(path)
            if target.parent == process and target.name == 'fd':
                raise PermissionError('fixture non-dumpable source')
            return original_entries(path, *args, **kwargs)
        # live_targets ignores a non-cargo identity, so the census that must
        # explain itself here is orphan_cache.active; ask it directly.
        # setUp stubs orphan_cache.active; this witness must run the real one,
        # saved as self.references before the stub replaced it.
        with patch.object(Path, 'open', hidden_open), \
                patch.object(os, 'readlink', hidden_link), \
                patch.object(hygiene.orphan_cache.targets, 'bounded_entries', hidden_entries):
            reasons = []
            self.assertIsNone(self.references(
                [self.cache/'candidate'], time.monotonic() + 20, self.proc,
                refused=reasons))
        message = '; '.join(reasons)
        self.assertIn('identity is unproven', message)
        self.assertNotIn('not the non-dumpable signature', message)

    def test_a_credential_beside_a_managed_path_never_reaches_the_refusal(self):
        # A process can hold a credential in any source the census reads. The
        # refusal names that source and the managed root it matched, and copies
        # neither the value nor any other part of the entry.
        process = self.proc/'63'; process.mkdir()
        (process/'task'/'63').mkdir(parents=True)
        (process/'task'/'63'/'children').write_text('')
        (process/'stat').write_bytes(b'63 (cargo) R ' + b'0 '*18 + b'1\n')
        (process/'comm').write_bytes(b'cargo\n')
        (process/'fd').mkdir()
        foreign = self.home/'foreign'; foreign.mkdir()
        (process/'cwd').symlink_to(foreign, target_is_directory=True)
        (process/'exe').symlink_to('/usr/bin/cargo')
        (process/'cmdline').write_bytes(b'cargo\0check\0')
        secret = 'fixture-credential-never-valid'
        managed = self.cache/'target'/'fixture'
        managed.mkdir(parents=True, exist_ok=True)
        # One compound value, as a build can hold: the credential and the
        # managed path are in the SAME entry, so a refusal that copies any part
        # of the value leaks the credential.
        compound = json.dumps({'token': secret, 'output': str(managed)},
                              separators=(',', ':'))
        (process/'environ').write_bytes(
            b'BUILD_FIXTURE_CREDENTIALS=' + os.fsencode(compound) + b'\0')
        (self.proc/'sys/kernel/random').mkdir(parents=True, exist_ok=True)
        (self.proc/'sys/kernel/random/boot_id').write_text('fixture-boot\n')
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured), contextlib.redirect_stderr(captured):
            with patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}):
                with self.assertRaises(RuntimeError) as raised:
                    hygiene.active_targets({}, time.monotonic() + 20, self.proc)
        text = str(raised.exception) + captured.getvalue()
        self.assertIn('environment variable BUILD_FIXTURE_CREDENTIALS', text,
                      'the refusal must name the source')
        self.assertIn(str(self.cache/'target'), text,
                      'the refusal must name the managed root it matched')
        self.assertNotIn(secret, text, 'the refusal must never copy the value')
        self.assertNotIn('token', text, 'no part of the value may be quoted')

    def test_process_derived_exception_text_never_reaches_the_refusal(self):
        # An OSError on a path taken from the process repeats that path in its
        # text, its filename and its args. The refusal may name the error type
        # and the errno, and never any of those.
        secret = 'fixture-secret-never-valid'
        cases = {}
        # A malformed CARGO_HOME naming a regular file.
        home = self.home/secret
        home.touch()
        cases['CARGO_HOME'] = (b'CARGO_HOME=' + os.fsencode(home) + b'\0',
                               [b'cargo', b'check'])
        # A malformed response-file argument under a regular file.
        blocker = self.home/'blocker'
        blocker.touch()
        cases['response'] = (b'',
                            [b'cargo', b'check',
                             b'@' + os.fsencode(blocker/secret)])
        for label, (environment, arguments) in cases.items():
            with self.subTest(case=label):
                pid = f'{6200 + len(label)}'
                process = self.proc/pid
                process.mkdir()
                (process/'task'/pid).mkdir(parents=True)
                (process/'task'/pid/'children').write_text('')
                (process/'stat').write_bytes(
                    os.fsencode(pid) + b' (cargo) R ' + b'0 '*18 + b'1\n')
                (process/'comm').write_bytes(b'cargo\n')
                (process/'fd').mkdir()
                (process/'cwd').symlink_to(self.home)
                (process/'exe').symlink_to('/usr/bin/cargo')
                (process/'cmdline').write_bytes(b'\0'.join(arguments) + b'\0')
                (process/'environ').write_bytes(environment or b'HOME=/fixture\0')
                (self.proc/'sys/kernel/random').mkdir(parents=True, exist_ok=True)
                (self.proc/'sys/kernel/random/boot_id').write_text('fixture-boot\n')
                captured = io.StringIO()
                with contextlib.redirect_stdout(captured), \
                        contextlib.redirect_stderr(captured):
                    with patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}):
                        with self.assertRaises(RuntimeError) as raised:
                            hygiene.active_targets({}, time.monotonic() + 20, self.proc)
                text = str(raised.exception) + captured.getvalue()
                self.assertIn('stopping before further target removal', text)
                self.assertNotIn(secret, text,
                                 'no filename or exception text may be copied')
                self.assertNotIn(str(home), text)

    def test_a_response_filename_never_reaches_the_refusal(self):
        # A valid response file under a process-supplied name still names the
        # managed root, so it refuses. The refusal says which argv position it
        # came from, because the filename itself is a process value.
        secret = 'fixture-secret-never-valid'
        pid = '6300'
        process = self.proc/pid
        process.mkdir()
        (process/'task'/pid).mkdir(parents=True)
        (process/'task'/pid/'children').write_text('')
        (process/'stat').write_bytes(
            os.fsencode(pid) + b' (cargo) R ' + b'0 '*18 + b'1\n')
        (process/'comm').write_bytes(b'cargo\n')
        (process/'fd').mkdir()
        (process/'cwd').symlink_to(self.home)
        (process/'exe').symlink_to('/usr/bin/cargo')
        response = self.home/secret
        response.write_bytes(os.fsencode(self.cache/'target'/'absent'))
        (process/'cmdline').write_bytes(
            b'cargo\0check\0@' + os.fsencode(response) + b'\0')
        (process/'environ').write_bytes(b'HOME=/fixture\0')
        (self.proc/'sys/kernel/random').mkdir(parents=True, exist_ok=True)
        (self.proc/'sys/kernel/random/boot_id').write_text('fixture-boot\n')
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured), \
                contextlib.redirect_stderr(captured):
            with patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}):
                with self.assertRaises(RuntimeError) as raised:
                    hygiene.active_targets({}, time.monotonic() + 20, self.proc)
        text = str(raised.exception) + captured.getvalue()
        self.assertIn('response file at argv[2]', text)
        self.assertNotIn(secret, text, 'the filename is a process value')
        self.assertNotIn(str(response), text)

    def test_a_transient_census_failure_restarts_observation_before_idle_cleanup(self):
        for interrupted in ('builds', 'references'):
            with self.subTest(interrupted=interrupted):
                idle, owner = self.target(interrupted+'-idle')
                live, _ = self.target(interrupted+'-live')
                calls = dict(builds=0, references=0)
                def census(which):
                    calls[which] += 1
                    if which == interrupted and calls[which] == 1:
                        return None
                    return {live} if which == 'builds' else set()
                with patch.object(evict_targets, 'live_targets', side_effect=lambda *_a, **_k: census('builds')), \
                        patch.object(hygiene.orphan_cache, 'active', side_effect=lambda *_a, **_k: census('references')):
                    self.assertEqual(hygiene.reclaim(self.cache, self.home, now=self.now, proc=self.proc), [idle])
                self.assertFalse(idle.exists())
                self.assertTrue(live.exists())
                self.assertEqual((owner/'source.rs').read_text(), 'keep source')
                shutil.rmtree(live)

    def test_activity_rechecked_after_acquiring_lease(self):
        target, _ = self.target('active-after-inventory')
        with patch.object(evict_targets,'live_targets',side_effect=[set(),{target}]):
            self.assertEqual(hygiene.reclaim(self.cache,self.home,now=self.now,proc=self.proc),[])
        self.assertTrue(target.exists())

    def test_a_target_removed_after_inventory_does_not_block_other_idle_cleanup(self):
        vanished, _ = self.target('removed-during-inventory')
        idle, owner = self.target('remaining-idle')
        def census(*_args, **_options):
            if vanished.exists():
                shutil.rmtree(vanished)
            return set()
        with patch.object(evict_targets, 'live_targets', side_effect=census):
            removed = hygiene.reclaim(self.cache, self.home, now=self.now, proc=self.proc)
        self.assertEqual(removed, [idle])
        self.assertFalse(idle.exists())
        self.assertEqual((owner/'source.rs').read_text(), 'keep source')

    def test_local_cargo_target_and_dry_run(self):
        owner=self.home/'rust-local'; (owner/'.git').mkdir(parents=True)
        (owner/'Cargo.toml').write_text('[workspace]')
        (owner/'crates/grapt-types').mkdir(parents=True)
        (owner/'crates/grapt-types/Cargo.toml').write_text('[package]\nname="grapt-types"')
        target=owner/'target'; (target/'debug').mkdir(parents=True)
        (target/'CACHEDIR.TAG').write_text('Cargo target')
        (target/'debug/artifact').write_text('compiled'); self.age(target,7200)
        self.assertEqual(self.reclaim(dry_run=True),[target]); self.assertTrue(target.exists())
        self.assertEqual(self.reclaim(),[target]); self.assertTrue((owner/'Cargo.toml').exists())

    def test_unrelated_rust_project_targets_are_not_ours(self):
        owner=self.home/'rust-unrelated'; (owner/'.git').mkdir(parents=True)
        (owner/'Cargo.toml').write_text('[workspace]')
        target=owner/'target'; (target/'debug').mkdir(parents=True)
        (target/'CACHEDIR.TAG').write_text('Cargo target'); self.age(target,7200)
        self.assertEqual(self.reclaim(),[])
        self.assertTrue(target.exists())

    def test_standard_cross_compilation_outputs_are_disposable(self):
        target, _ = self.target('cross-build')
        output=target/'x86_64-unknown-linux-gnu/debug'
        output.mkdir(parents=True); (output/'artifact').write_text('compiled')
        self.age(target,7200)
        self.assertEqual(self.reclaim(),[target])

    def test_symlink_target_cannot_remove_external_content(self):
        target, _ = self.target('symlink')
        external=self.home/'outside'; external.mkdir(); (external/'data').write_text('keep')
        import shutil
        shutil.rmtree(target); target.symlink_to(external,target_is_directory=True)
        self.assertEqual(self.reclaim(),[])
        self.assertEqual((external/'data').read_text(),'keep')

    def test_landed_target_can_be_removed_recently_but_new_build_clears_marker(self):
        target, owner=self.target('finished',age=60)
        subprocess.run(['git','init','-q',str(owner)],check=True)
        subprocess.run(['git','-C',str(owner),'add','--','source.rs'],check=True)
        subprocess.run(['git','-C',str(owner),'-c','user.name=Test','-c','user.email=test@localhost',
                        'commit','-qm','fixture','--','source.rs'],check=True)
        commit=subprocess.check_output(['git','-C',str(owner),'rev-parse','HEAD'],text=True).strip()
        hygiene.mark_finished(target,owner,commit)
        with patch.object(evict_targets,'evict'):
            held=evict_targets.prepare(target.parent.parent,target,owner,proc=self.proc); held.close()
        self.assertFalse((target/hygiene.FINISHED).exists())
        hygiene.mark_finished(target,owner,commit)
        self.assertEqual(self.reclaim(),[target])

    def test_cleanup_has_its_own_maintenance_window_and_failure_is_not_success(self):
        target, _=self.target('slow-delete')
        actual_run=hygiene.subprocess.run
        observed=[]
        def slow_delete(command, **options):
            if command[0]=='rm':
                observed.append(options['timeout'])
                raise subprocess.TimeoutExpired(command,options['timeout'])
            return actual_run(command,**options)
        with patch.object(hygiene.subprocess,'run',side_effect=slow_delete):
            with self.assertRaises(subprocess.TimeoutExpired):self.reclaim()
        self.assertEqual(len(observed),1)
        self.assertGreater(observed[0],60)
        self.assertTrue(target.exists())

    def test_finished_orphan_is_reclaimed_without_an_idle_delay(self):
        target, owner = self.target('retired-owner', age=60)
        shutil.rmtree(owner)
        (target/hygiene.FINISHED).write_text('a'*40+'\n')
        self.assertEqual(self.reclaim(), [target])

    def test_later_output_invalidates_an_orphan_finished_marker(self):
        target, owner = self.target('retired-rebuilt', age=60)
        shutil.rmtree(owner)
        marker = target/hygiene.FINISHED
        marker.write_text('a'*40+'\n')
        os.utime(marker, (self.now-60, self.now-60))
        (target/'debug/artifact').write_text('new build')
        self.assertEqual(self.reclaim(), [])
        self.assertTrue(target.exists())

    def test_recent_idle_target_prunes_old_hashes_but_keeps_latest(self):
        target, _ = self.target('warm-target', age=60)
        deps = target/'debug/deps'; deps.mkdir()
        old = deps/('journey-'+'1'*16); old.write_bytes(b'\x7fELFgenerated'); old.chmod(0o755)
        os.utime(old, (self.now-7200, self.now-7200))
        newest = deps/('journey-'+'2'*16); newest.write_bytes(b'\x7fELFgenerated'); newest.chmod(0o755)
        self.assertEqual(self.reclaim(), [])
        self.assertFalse(old.exists()); self.assertTrue(newest.exists())

    def test_live_owner_prunes_superseded_generations_only_between_builds(self):
        target, owner = self.target('working-owner')
        deps = target/'debug/deps'; deps.mkdir()
        old = deps/('journey-'+'1'*16); old.write_bytes(b'\x7fELFgenerated'); old.chmod(0o755)
        newest = deps/('journey-'+'2'*16); newest.write_bytes(b'\x7fELFgenerated'); newest.chmod(0o755)
        os.utime(old, (self.now-7200,)*2)
        generations = []
        for name, age in [('aaaa', 9000), ('bbbb', 8000), ('cccc', 7000)]:
            path = target/'debug/incremental'/('journey-'+name)
            path.mkdir(parents=True); (path/'work-products.bin').write_bytes(b'compiled')
            self.age(path, age); generations.append(path)
        # No live process is needed to prove this: the guards below are the
        # lease, an open reader and the Cargo lock, and each is a real
        # condition the pass must respect. The empty table keeps the census off
        # the host entirely.
        proc = self.home/'test-proc'; proc.mkdir(exist_ok=True)
        scan = evict_targets.live_targets
        with patch.object(evict_targets, 'live_targets',
                          side_effect=lambda owners, proc=None, deadline=None, **options:
                          scan(owners, proc, deadline, **options)), \
                patch.object(hygiene.orphan_cache, 'active',
                             side_effect=lambda paths, deadline, proc, refused=None:
                             self.references(paths, deadline, proc=proc)):
            with evict_targets.lease(target, shared=True):
                self.assertEqual(hygiene.reclaim(self.cache, self.home, now=self.now,
                                                 proc=proc), [])
                self.assertTrue(old.exists())
            # A synthetic reader of `old`: the census sees the descriptor, so the
            # generation it names must survive while it is held.
            reader = proc/'7000'
            (reader/'fd').mkdir(parents=True)
            (reader/'task'/'7000').mkdir(parents=True)
            (reader/'stat').write_bytes(b'7000 (bash) S ' + b'0 ' * 18 + b'99\n')
            (reader/'task'/'7000'/'children').write_text('')
            (reader/'comm').write_text('bash\n')
            (reader/'exe').symlink_to('/fixture/bash')
            (reader/'cwd').symlink_to(target/'debug', target_is_directory=True)
            (reader/'environ').write_bytes(b'HOME=/fixture\0')
            (reader/'cmdline').write_bytes(b'bash\0')
            (reader/'fd'/'3').symlink_to(old)
            try:
                self.assertEqual(hygiene.reclaim(self.cache, self.home, now=self.now,
                                                 proc=proc), [])
                self.assertTrue(old.exists())
            finally:
                import shutil; shutil.rmtree(reader)
            with (target/'debug/.cargo-lock').open('a') as cargo:
                fcntl.flock(cargo, fcntl.LOCK_EX | fcntl.LOCK_NB)
                self.assertEqual(hygiene.reclaim(self.cache, self.home, now=self.now,
                                                 proc=proc), [])
                self.assertTrue(old.exists())
            self.assertEqual(hygiene.reclaim(self.cache, self.home, now=self.now,
                                             proc=proc), [])
            self.assertFalse(old.exists()); self.assertTrue(newest.exists())
            self.assertFalse(generations[0].exists())
            self.assertTrue(all(path.exists() for path in generations[1:]))
            self.assertTrue((owner/'source.rs').is_file())

    def test_open_reference_guard_preserves_a_target_even_without_cargo(self):
        target, _ = self.target('read-by-someone')
        with patch.object(evict_targets, 'live_targets', return_value=set()), \
                patch.object(hygiene.orphan_cache, 'active', return_value={target}):
            self.assertEqual(hygiene.reclaim(self.cache, self.home, now=self.now, proc=self.proc), [])
        self.assertTrue(target.exists())

    def test_changed_owner_or_new_unknown_content_after_inventory_preserves_target(self):
        for change in ('owner', 'content'):
            with self.subTest(change=change):
                target, _ = self.target('changed-' + change)
                def changed(owners, deadline):
                    if change == 'owner':
                        (target/'.secode-worktree').write_text(str(self.home/'different'))
                    else:
                        (target/'source-data').write_text('preserve')
                    return set()
                scans = iter([False, True])
                def activity(owners, deadline, proc=None):
                    return changed(owners, deadline) if next(scans) else set()
                with patch.object(hygiene, 'active_targets', side_effect=activity):
                    self.assertEqual(hygiene.reclaim(self.cache, self.home, now=self.now,
                                                     proc=self.proc), [])
                self.assertTrue(target.exists())
                shutil.rmtree(target)

    def test_a_later_pass_refusal_still_leaves_the_decisions_already_made(self):
        # The journal is evidence. A refusal from a later pass must not erase the
        # decisions this pass already reached, or a refusal looks like a no-op.
        old, _ = self.target('old')
        with patch.object(hygiene.orphan_cache, 'reclaim',
                          side_effect=RuntimeError('Unknown cache process')):
            with self.assertRaises(RuntimeError):
                self.reclaim()
        self.assertFalse(old.exists())
        rows = json.loads((self.logs/'target-clean.json').read_text())['samples']
        removed = [row for row in rows if row['outcome'] == 'removed']
        self.assertEqual(len(removed), 1, rows)
        self.assertGreater(removed[0]['bytes_before'], 0)
        self.assertEqual(removed[0]['bytes_after'], 0)

    def test_an_unrecognised_target_kind_is_counted_and_journalled_but_never_deleted(self):
        idle, _ = self.target('idle-target')
        # A kind no pass owns, holding real bytes.
        kind = self.cache/'target/toolchain/quarantined'
        (kind/'slot').mkdir(parents=True)
        (kind/'slot/artifact').write_bytes(b'x'*4096)
        self.reclaim()
        self.assertFalse(idle.exists())
        # Nothing about an unrecognised kind licenses deleting it.
        self.assertTrue((kind/'slot/artifact').is_file())
        rows = json.loads((self.logs/'target-clean.json').read_text())['samples']
        unowned = [row for row in rows if row['outcome'] == 'unrecognised']
        self.assertEqual([row['target'] for row in unowned], [str(kind)])
        self.assertGreater(unowned[0]['bytes_before'], 0)
        self.assertIn('never deleted', unowned[0]['detail'])

    def test_supported_legacy_layouts_are_not_unrecognised_kinds(self):
        for layout in ('flat-worktrees', 'shared-toolchain'):
            with self.subTest(layout=layout):
                target, owner = self.target(layout, age=60)
                if layout == 'flat-worktrees':
                    destination = self.cache/'target/worktrees'/target.name
                    destination.parent.mkdir(exist_ok=True)
                else:
                    destination = self.cache/'target/1.97.1'
                    (target/'CACHEDIR.TAG').write_text('Cargo target')
                target.rename(destination)
                self.assertIn(destination, hygiene.discover(
                    self.cache, self.home, time.monotonic()+10))
                self.reclaim()
                rows = json.loads((self.logs/'target-clean.json').read_text())['samples']
                unrecognised = [row for row in rows if row['outcome'] == 'unrecognised'
                                and Path(row['target']).is_relative_to(destination)]
                self.assertEqual(unrecognised, [])

    def test_kind_telemetry_stops_when_the_maintenance_deadline_has_expired(self):
        kind = self.cache/'target/toolchain/quarantined'
        kind.mkdir(parents=True)
        with patch.object(hygiene, 'measured', return_value=4096) as measured:
            try:
                hygiene.unrecognised_kinds(self.cache, [], time.monotonic()-1, False)
            except RuntimeError:
                pass
        measured.assert_not_called()

    def test_a_dry_run_records_nothing(self):
        old, _ = self.target('old')
        self.reclaim(dry_run=True)
        self.assertTrue(old.exists())
        self.assertFalse((self.logs/'target-clean.json').exists())

    def test_main_coalesces_an_overlapping_maintenance_request(self):
        with (self.cache/'.hygiene.lock').open('a') as held, \
                patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}), \
                patch.object(sys, 'argv', ['disk_hygiene.py']), \
                patch.object(hygiene, 'reclaim') as reclaim, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            fcntl.flock(held, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.assertEqual(hygiene.main(), 0)
        reclaim.assert_not_called()
        self.assertIn('coalesced', output.getvalue())

    def test_finish_and_disk_pressure_request_maintenance_without_waiting_for_a_build_slot(self):
        target, owner = self.target('finish-request')
        commit = 'b'*40
        with patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache), 'CARGO_TARGET_DIR': str(target)}), \
                patch.object(sys, 'argv', ['disk_hygiene.py', '--finish', commit]), \
                patch.object(hygiene, 'mark_finished') as mark, \
                patch.object(hygiene.subprocess, 'run') as request:
            self.assertEqual(hygiene.main(), 0)
        mark.assert_called_once_with(target, Path.cwd().resolve(), commit)
        request.assert_called_once_with(['systemctl', '--user', 'start', '--no-block',
                                        'secode-disk-hygiene.service'], check=True, timeout=10)
        for cleanup_fails in (False, True):
            with self.subTest(cleanup_fails=cleanup_fails), \
                    patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}), \
                    patch.object(sys, 'argv', ['disk_hygiene.py', '--alert-only', '--agent-id', 'agt-fixture-seat']), \
                    patch.object(shutil, 'disk_usage') as space, \
                    patch.object(hygiene.disk_stall, 'capacity_warning',
                        side_effect=None if cleanup_fails else RuntimeError('mail unavailable')) as warning, \
                    patch.object(hygiene.subprocess, 'run',
                        side_effect=subprocess.CalledProcessError(1, ['systemctl']) if cleanup_fails else None) as request, \
                    contextlib.redirect_stderr(io.StringIO()) as output:
                space.return_value.free = hygiene.disk_stall.CAPACITY_WARNING_BYTES-1
                self.assertEqual(hygiene.main(), 1)
            warning.assert_called_once_with(self.cache, 'agt-fixture-seat')
            request.assert_called_once_with(['systemctl', '--user', 'start', '--no-block',
                                            'secode-disk-hygiene.service'], check=True, timeout=10)
            self.assertIn('Cleanup request failed' if cleanup_fails else 'Disk warning failed', output.getvalue())

    def test_scheduled_cleanup_retains_over_cap_measurement_and_cleanup_failure(self):
        diagnostics = self.home/'diagnostics'
        for fails in (False, True):
            with self.subTest(fails=fails), \
                    patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}), \
                    patch.object(sys, 'argv', ['disk_hygiene.py', '--diagnostics', str(diagnostics)]), \
                    patch.object(hygiene, 'reclaim', side_effect=RuntimeError('unknown reader') if fails else None), \
                    patch.object(evict_targets, 'usage', return_value=evict_targets.CAP_BYTES+1), \
                    patch.object(shutil, 'disk_usage') as disk, \
                    contextlib.redirect_stderr(io.StringIO()):
                disk.return_value.free = 1024**4
                self.assertEqual(hygiene.main(), 1 if fails else 0)
            row = json.loads(max(diagnostics.iterdir(), key=lambda p:p.stat().st_mtime_ns).read_text())
            self.assertEqual(row['cap_bytes'], evict_targets.CAP_BYTES)
            self.assertEqual(row['whole_cache_bytes'], evict_targets.CAP_BYTES+1)
            self.assertEqual(row['over_budget_bytes'], 1)
            self.assertEqual(row['error'], 'unknown reader' if fails else None)
        with patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}), \
                patch.object(sys, 'argv', ['disk_hygiene.py', '--alert-only', '--agent-id', 'agt-fixture-seat']), \
                patch.object(evict_targets, 'usage',
                             side_effect=RuntimeError('du timed out after 30 seconds')), \
                patch.object(shutil, 'disk_usage') as disk, \
                patch.object(hygiene.disk_stall, 'capacity_warning'), \
                patch.object(hygiene.subprocess, 'run') as request, \
                contextlib.redirect_stderr(io.StringIO()):
            disk.return_value.free = hygiene.disk_stall.CAPACITY_WARNING_BYTES
            self.assertEqual(hygiene.main(), 0)
        request.assert_not_called()

    def test_census_timeout_reports_unknown_but_other_failures_still_fail(self):
        diagnostics = self.home/'diagnostics'
        for timeout in (subprocess.TimeoutExpired('du', 30),
                        RuntimeError('Target cache maintenance exceeded its time limit')):
            with self.subTest(timeout=type(timeout).__name__), \
                    patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}), \
                    patch.object(sys, 'argv', ['disk_hygiene.py', '--diagnostics', str(diagnostics)]), \
                    patch.object(hygiene, 'reclaim') as reclaim, \
                    patch.object(evict_targets, 'usage', side_effect=timeout), \
                    patch.object(shutil, 'disk_usage') as disk, \
                    contextlib.redirect_stdout(io.StringIO()) as output, \
                    contextlib.redirect_stderr(io.StringIO()):
                disk.return_value.free = 1024**4
                self.assertEqual(hygiene.main(), 0)
            reclaim.assert_called_once()
            row = json.loads(max(diagnostics.iterdir(), key=lambda p:p.stat().st_mtime_ns).read_text())
            self.assertEqual(row['whole_cache_bytes'], 'unknown')
            self.assertEqual(row['over_budget_bytes'], None)
            self.assertEqual(row['error'], None)
            self.assertIn('excess=unknown', output.getvalue())
        with patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}), \
                patch.object(sys, 'argv', ['disk_hygiene.py', '--diagnostics', str(diagnostics)]), \
                patch.object(hygiene, 'reclaim'), \
                patch.object(evict_targets, 'usage',
                             side_effect=PermissionError('unreadable cache')), \
                patch.object(shutil, 'disk_usage') as disk, \
                contextlib.redirect_stderr(io.StringIO()):
            disk.return_value.free = 1024**4
            self.assertEqual(hygiene.main(), 1)
        row = json.loads(max(diagnostics.iterdir(), key=lambda p:p.stat().st_mtime_ns).read_text())
        self.assertIsNone(row['whole_cache_bytes'])
        self.assertIn('unreadable cache', row['error'])

    def test_alert_only_pressures_from_statvfs_on_each_watched_filesystem(self):
        cases = [
            (hygiene.disk_stall.CAPACITY_WARNING_BYTES-1, hygiene.disk_stall.CAPACITY_ALERT_BYTES, True),
            (hygiene.disk_stall.CAPACITY_WARNING_BYTES, hygiene.disk_stall.CAPACITY_ALERT_BYTES-1, True),
            (hygiene.disk_stall.CAPACITY_WARNING_BYTES-1, hygiene.disk_stall.CAPACITY_ALERT_BYTES-1, True),
            (hygiene.disk_stall.CAPACITY_WARNING_BYTES, hygiene.disk_stall.CAPACITY_ALERT_BYTES, False),
        ]
        for cache_free, root_free, pressure in cases:
            with self.subTest(cache_free=cache_free, root_free=root_free), \
                    patch.dict(os.environ, {'SECODE_CACHE_DIR': str(self.cache)}), \
                    patch.object(sys, 'argv', ['disk_hygiene.py', '--alert-only', '--agent-id', 'agt-fixture-seat']), \
                    patch.object(shutil, 'disk_usage', side_effect=lambda path: type(
                        'usage', (), {'free': root_free if Path(path) == Path('/') else cache_free})()), \
                    patch.object(hygiene.disk_stall, 'capacity_warning') as warning, \
                    patch.object(hygiene.subprocess, 'run') as request, \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(hygiene.main(), 0)
            if pressure:
                request.assert_called_once_with(['systemctl', '--user', 'start', '--no-block',
                                                'secode-disk-hygiene.service'], check=True, timeout=10)
            else:
                request.assert_not_called()
            warning.assert_called_once_with(self.cache, 'agt-fixture-seat')


    def test_a_malformed_journal_never_fails_completed_cleanup(self):
        target, _ = self.target('damaged-journal')
        self.logs.mkdir()
        row = hygiene.clean_journal.row('/previous', 'retained')
        row['outcome'] = []
        journal = self.logs/'target-clean.json'
        journal.write_text(json.dumps({'version': 1, 'samples': [row]}))
        original = journal.read_bytes()
        self.assertEqual(self.reclaim(), [target])
        self.assertFalse(target.exists())
        self.assertEqual(journal.read_bytes(), original)

    def test_telemetry_size_failure_does_not_prevent_idle_cleanup(self):
        target, _ = self.target('unmeasured-idle')
        with patch.object(evict_targets, 'usage', side_effect=RuntimeError('measurement unavailable')):
            self.assertEqual(self.reclaim(), [target])
        self.assertFalse(target.exists())

    def test_activity_found_under_the_lease_is_recorded_as_live(self):
        target, _ = self.target('became-active')
        with patch.object(hygiene, 'active_targets', side_effect=[set(), {target}]):
            self.assertEqual(hygiene.reclaim(self.cache, self.home, now=self.now, proc=self.proc), [])
        self.assertTrue(target.exists())
        rows = json.loads((self.logs/'target-clean.json').read_text())['samples']
        self.assertEqual([(row['target'], row['outcome']) for row in rows], [(str(target), 'live')])


    def test_measurement_timeout_cannot_spend_the_removal_deadline(self):
        target, _ = self.target('slow-measurement')
        clock = [time.monotonic()]
        def timeout(_target, deadline):
            clock[0] = deadline + 0.01
            raise subprocess.TimeoutExpired(['du'], 1)
        with patch.object(time, 'monotonic', side_effect=lambda: clock[0]), \
                patch.object(evict_targets, 'usage', side_effect=timeout):
            self.assertEqual(self.reclaim(), [target])
        self.assertFalse(target.exists())

    def test_dry_runs_never_measure_either_recent_or_idle_targets(self):
        self.target('dry-idle')
        self.target('dry-recent', age=60)
        with patch.object(evict_targets, 'usage', return_value=4096) as measure:
            self.reclaim(dry_run=True)
        measure.assert_not_called()
