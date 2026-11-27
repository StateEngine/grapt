"""Exercise one-off reclamation without touching the real cache or worktrees."""
import contextlib
import io
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import orphan_cache


class OrphanCacheTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = Path(self.temp.name)
        self.cache = self.home/'.cache/secode'
        self.cache.mkdir(parents=True)
        self.now = time.time()
        # An empty process table, passed explicitly so no path can read the host.
        self.proc = self.home/'proc'; self.proc.mkdir(exist_ok=True)

    def directory(self, name, age=7200):
        path = self.cache/name
        path.mkdir()
        (path/'result.log').write_text('derived evidence')
        self.age(path, age)
        return path

    def age(self, path, age):
        for entry in [*path.rglob('*'), path]:
            os.utime(entry, (self.now-age, self.now-age), follow_symlinks=False)

    def reclaim(self, owners=None, dry_run=False):
        with patch.object(orphan_cache, 'active', return_value=set()):
            return orphan_cache.reclaim(self.cache, self.home, owners or {}, dry_run,
                                        self.now, time.monotonic()+10, proc=self.proc)

    def test_an_approved_service_with_a_hidden_cwd_is_blind(self):
        used = self.directory('working-evidence')
        self.assertIsNone(self.hidden_process(
            [b'/usr/bin/ssh-agent'], b'ssh-agent', '/usr/bin/ssh-agent',
            {'cwd'}, cwd=used, paths=[used]))

    def test_a_hidden_executable_is_blind_for_every_process(self):
        for argv, comm in [([b'python', b'work'], b'python'),
                           ([b'/usr/bin/ssh-agent'], b'ssh-agent')]:
            with self.subTest(comm=comm):
                self.assertIsNone(self.hidden_process(argv, comm, None, set()))

    def test_a_service_named_program_does_not_clear_its_hidden_descriptors(self):
        visible = self.directory('visible-evidence')
        hidden = self.directory('descriptor-evidence')
        executable = self.home/'ssh-agent'
        shutil.copy2(sys.executable, executable)
        with (hidden/'result.log').open() as handle:
            descriptor = handle.fileno()
            process = subprocess.Popen([
                'ssh-agent', '-c', 'import os,sys; f=os.fdopen(int(sys.argv[1])); '
                'print("ready",flush=True); sys.stdin.readline()', str(descriptor)],
                executable=executable, pass_fds=(descriptor,), cwd=visible,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        try:
            self.assertEqual(process.stdout.readline(), b'ready\n')
            proc = self.home/'test-proc'; proc.mkdir(exist_ok=True)
            entry = proc/str(process.pid)
            entry.symlink_to(f'/proc/{process.pid}', target_is_directory=True)
            self.assertEqual((entry/'comm').read_text().strip(), 'ssh-agent')
            self.assertEqual(Path(os.readlink(entry/'exe')), executable)
            entries = orphan_cache.targets.bounded_entries
            scanner = orphan_cache.active

            def partly_hidden(path, *args, **kwargs):
                if Path(path) == entry/'fd':
                    raise PermissionError('fixture descriptor census')
                return entries(path, *args, **kwargs)

            with patch.object(orphan_cache.targets, 'bounded_entries', partly_hidden), \
                    patch.object(orphan_cache, 'active', side_effect=lambda paths, deadline, proc, refused=None:
                                 scanner(paths, deadline, proc)):
                orphan_cache.reclaim(self.cache, self.home, {}, False,
                                     self.now, time.monotonic()+10, proc=proc)
            self.assertTrue(hidden.exists(), 'the generic program still holds its file')
        finally:
            process.terminate(); process.wait(timeout=5)
            process.stdin.close(); process.stdout.close()

    def hidden_process(self, arguments, comm, executable, hidden, cwd=None, paths=None):
        """Model non-dumpable /proc entries without changing host permissions."""
        # The caller names the descriptor source either way it reads naturally.
        hidden = {'descriptors' if source == 'fd' else source for source in hidden}
        with tempfile.TemporaryDirectory(dir=self.home) as directory:
            proc = Path(directory)
            process = proc/'123'; process.mkdir()
            (process/'stat').write_text('123 (fixture) S 1 1')
            (process/'comm').write_bytes(comm+b'\n')
            (process/'cmdline').write_bytes(b'\0'.join(arguments)+b'\0')
            (process/'environ').write_bytes(b'HOME=/fixture\0')
            (process/'fd').mkdir()
            (process/'exe').symlink_to(executable or '/unreadable')
            (process/'cwd').symlink_to(cwd or self.home)
            original_open, original_link = Path.open, os.readlink
            original_entries = orphan_cache.targets.bounded_entries

            def open_file(path, *args, **kwargs):
                if path.name in hidden and path.parent == process:
                    raise PermissionError('fixture process metadata')
                return original_open(path, *args, **kwargs)

            def read_link(path, *args, **kwargs):
                if Path(path) == process/'exe' and ('exe' in hidden or executable is None):
                    raise PermissionError('fixture executable')
                if Path(path) == process/'cwd' and 'cwd' in hidden:
                    raise PermissionError('fixture working directory')
                return original_link(path, *args, **kwargs)

            def entries(path, *args, **kwargs):
                if Path(path) == process/'fd' and 'descriptors' in hidden:
                    raise PermissionError('fixture descriptors')
                return original_entries(path, *args, **kwargs)

            with patch.object(Path, 'open', open_file), patch.object(os, 'readlink', read_link), \
                    patch.object(orphan_cache.targets, 'bounded_entries', entries):
                return orphan_cache.active(paths or [self.cache/'idle-evidence'],
                                           time.monotonic()+10, proc)

    def test_approved_services_are_exempt_only_when_every_gated_source_is_hidden(self):
        # These paths/argv identities were observed on the workstation. The sshd
        # title and basename-agent variants are the explicit operator contract.
        # A name is not an identity: the exemption is the all-hidden kernel
        # signature of a non-dumpable process, so a partial hide is blind.
        identities = [
            ([b'/usr/lib/systemd/systemd', b'--user'], b'systemd', '/usr/lib/systemd/systemd'),
            ([b'(sd-pam)'], b'(sd-pam)', '/usr/lib/systemd/systemd'),
            ([b'/usr/bin/ssh-agent', b'-D'], b'ssh-agent', '/usr/bin/ssh-agent'),
            ([b'ssh-agent'], b'ssh-agent', '/usr/bin/ssh-agent'),
            ([b'sshd-session: fixture@pts/1'], b'sshd-session', '/usr/lib/openssh/sshd-session'),
        ]
        for arguments, comm, executable in identities:
            with self.subTest(arguments=arguments, all_hidden=True):
                self.assertEqual(
                    self.hidden_process(arguments, comm, executable,
                                        {'environ', 'fd', 'cwd', 'exe'}), set())
            for partial in ({'environ'}, {'fd'}, {'environ', 'fd'}, {'cwd'}):
                with self.subTest(arguments=arguments, partial=partial):
                    self.assertIsNone(
                        self.hidden_process(arguments, comm, executable, partial))

    def test_unknown_lookalike_and_mismatched_processes_spare_the_pass(self):
        # An unattributable process is never evidence for deleting anything, and
        # it may not abort: every candidate is spared for this pass instead.
        identities = [
            ([b'node'], b'MainThread', None),
            ([b'python', b'ssh-agent-lookalike'], b'ssh-agent', None),
            ([b'/tmp/ssh-agent'], b'ssh-agent', None),
            ([b'/usr/bin/ssh-agent-lookalike'], b'ssh-agent', None),
            ([b'/usr/bin/ssh-agent'], b'MainThread', None),
            ([b'/usr/bin/ssh-agent'], b'ssh-agent\n', None),
            ([b'/usr/bin/ssh-agent'], b'ssh-agent', '/usr/bin/node'),
            ([b'/usr/lib/systemd/systemd'], b'systemd', None),
            ([b'/usr/lib/systemd/systemd', b'--system', b'--user'], b'systemd', None),
            ([b'/tmp/systemd', b'--user'], b'systemd', None),
            ([b'sshd-session-lookalike: fixture'], b'sshd-session', None),
            ([b'sshd-session: fixture'], b'MainThread', None),
        ]
        for arguments, comm, executable in identities:
            for hidden in [{'environ'}, {'fd'}]:
                with self.subTest(arguments=arguments, hidden=hidden):
                    self.assertIsNone(self.hidden_process(arguments, comm, executable, hidden))
        self.assertIsNone(self.hidden_process([b'/usr/bin/ssh-agent'], b'ssh-agent',
                                             '/usr/bin/ssh-agent', {'cmdline'}))

    def test_approved_service_with_a_readable_source_is_scanned_not_exempted(self):
        # A partial hide is blind even for a named service: only the all-hidden
        # signature is the exemption. A readable cwd therefore protects what it
        # names, and the opaque rest makes the process blind rather than exempt.
        path = self.cache/'idle-evidence'
        self.assertIsNone(self.hidden_process([b'/usr/bin/ssh-agent'], b'ssh-agent',
                                             None, {'environ', 'fd'}, cwd=path))

    def test_exit_during_observation_is_skipped_only_with_a_kernel_exit_witness(self):
        for state in ('Z', 'X', 'S', 'unreadable'):
            with self.subTest(state=state), tempfile.TemporaryDirectory(dir=self.home) as temporary:
                proc = Path(temporary)
                process = proc/'123'; process.mkdir()
                (process/'stat').write_text('123 (fixture) S 1 1')
                (process/'task/123').mkdir(parents=True)
                (process/'cwd').symlink_to(self.home)
                original = os.readlink
                def leave(path, *args, **kwargs):
                    if Path(path) == process/'exe':
                        if state == 'unreadable':
                            (process/'stat').unlink()
                        else:
                            (process/'stat').write_text(f'123 (fixture) {state} 1 1')
                        raise FileNotFoundError('fixture exe disappeared')
                    return original(path, *args, **kwargs)
                with patch.object(os, 'readlink', leave):
                    actual = orphan_cache.active([self.cache], time.monotonic()+5, proc)
                if state in ('Z', 'X'):
                    self.assertEqual(actual, set())
                else:
                    # An unreadable process spares every candidate rather than
                    # aborting the pass for the whole cache.
                    self.assertIsNone(actual)

    def test_dead_main_thread_does_not_exempt_live_siblings_or_unreadable_thread_state(self):
        for siblings in (True, False):
            with self.subTest(siblings=siblings), tempfile.TemporaryDirectory(dir=self.home) as temporary:
                proc = Path(temporary)
                process = proc/'123'; process.mkdir()
                (process/'stat').write_text('123 (fixture) Z 1 1')
                if siblings:
                    (process/'task/123').mkdir(parents=True)
                    (process/'task/124').mkdir()
                    (process/'task/124/stat').write_text('124 (fixture) S 1 1')
                self.assertIsNone(orphan_cache.active([self.cache], time.monotonic()+5, proc))

    def test_idle_one_offs_removed_but_recent_unknown_and_outside_survive(self):
        old = self.directory('seat-evidence')
        dated = self.directory('trial-20261007')
        recent = self.directory('recent-evidence', age=60)
        unknown = self.directory('sccache')
        outside = self.home/'outside-evidence'; outside.mkdir()
        self.assertEqual(set(self.reclaim()), {old, dated})
        self.assertTrue(all(path.exists() for path in [recent,unknown,outside]))
        self.assertEqual(orphan_cache.reclaim(self.home,self.home,{},False,self.now,
                                              time.monotonic()+10, proc=self.proc), [])

    def test_repositories_owners_weights_and_links_are_preserved(self):
        repo = self.directory('repo-evidence'); (repo/'.git').mkdir()
        owned = self.directory('owned-evidence')
        model = self.directory('model-evidence'); (model/'qwen.gguf').write_text('weights')
        named = self.directory('qwen-evidence')
        external = self.home/'weights'; external.mkdir(); (external/'data').write_text('keep')
        link = self.cache/'link-evidence'; link.symlink_to(external, target_is_directory=True)
        for path in [repo,owned,model,named]: self.age(path,7200)
        self.assertEqual(self.reclaim({self.cache/'target':owned/'checkout'}), [])
        self.assertEqual((external/'data').read_text(), 'keep')
        self.assertTrue(all(path.exists() for path in [repo,owned,model,named,link]))

    def test_nested_recent_write_and_changed_activity_prevent_deletion(self):
        recent = self.directory('nested-evidence')
        os.utime(recent/'result.log',(self.now,self.now))
        active = self.directory('active-evidence')
        with patch.object(orphan_cache,'active',side_effect=[set(),{active}]):
            self.assertEqual(orphan_cache.reclaim(self.cache,self.home,{},False,self.now,
                                                  time.monotonic()+10, proc=self.proc), [])
        self.assertTrue(recent.exists()); self.assertTrue(active.exists())
        # An unattributable process spares everything and still returns a result;
        # it used to abort the whole pass with "Unknown cache process".
        with patch.object(orphan_cache,'active',return_value=None):
            self.assertEqual(orphan_cache.reclaim(self.cache,self.home,{},False,self.now,
                                                  time.monotonic()+10, proc=self.proc), [])
        self.assertTrue(recent.exists())

    def test_real_queued_process_argument_protects_directory_with_foreign_cwd(self):
        path = self.directory('queued-evidence')
        process = subprocess.Popen([sys.executable,'-c',
                                   'import time; print("ready",flush=True); time.sleep(30)',
                                   str(path/'job.sh')], cwd=self.home, stdout=subprocess.PIPE)
        try:
            self.assertEqual(process.stdout.readline(),b'ready\n')
            proc = self.home/'test-proc'; proc.mkdir(exist_ok=True)
            (proc/str(process.pid)).symlink_to(f'/proc/{process.pid}',target_is_directory=True)
            self.assertEqual(orphan_cache.active([path],time.monotonic()+10,proc),{path})
        finally:
            process.terminate(); process.wait(timeout=5); process.stdout.close()

    def test_a_partly_readable_process_spares_the_whole_pass(self):
        # A hidden working directory can name a candidate this process is using,
        # so a readable argument never licenses deleting the rest. A live child
        # holding its cwd in one candidate and an open file in another is the
        # case that matters: partial readability must never permit a deletion.
        used = self.directory('blinded-evidence')
        spare = self.directory('unrelated-evidence')
        self.assertIsNone(
            self.hidden_process([b'python', os.fsencode(used)], b'python', '/usr/bin/python',
                                {'environ', 'cwd'}, cwd=used, paths=[used, spare]))

    def test_a_process_with_nothing_readable_spares_every_candidate(self):
        used = self.directory('blinded-evidence')
        spare = self.directory('unrelated-evidence')
        self.assertIsNone(
            self.hidden_process([b'python', b'work'], b'python', None,
                                {'environ', 'cwd'}, cwd=used, paths=[used, spare]))

    def test_a_readable_reference_does_not_clear_an_unreadable_descriptor(self):
        visible = self.directory('visible-evidence')
        hidden = self.directory('descriptor-evidence')
        with (hidden/'result.log').open() as handle:
            descriptor = handle.fileno()
            process = subprocess.Popen([
                sys.executable, '-c',
                'import os,sys; f=os.fdopen(int(sys.argv[1])); '
                'print("ready",flush=True); sys.stdin.readline(); '
                'print(f.read(),flush=True)', str(descriptor)],
                pass_fds=(descriptor,), cwd=visible,
                stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        try:
            self.assertEqual(process.stdout.readline(), b'ready\n')
            proc = self.home/'test-proc'; proc.mkdir(exist_ok=True)
            entry = proc/str(process.pid)
            entry.symlink_to(f'/proc/{process.pid}', target_is_directory=True)
            self.assertEqual(Path(os.readlink(entry/'fd'/str(descriptor))),
                             hidden/'result.log')
            entries = orphan_cache.targets.bounded_entries
            scanner = orphan_cache.active

            def partly_hidden(path, *args, **kwargs):
                if Path(path) == entry/'fd':
                    raise PermissionError('fixture descriptor census')
                return entries(path, *args, **kwargs)

            with patch.object(orphan_cache.targets, 'bounded_entries', partly_hidden), \
                    patch.object(orphan_cache, 'active', side_effect=lambda paths, deadline, proc, refused=None:
                                 scanner(paths, deadline, proc)):
                orphan_cache.reclaim(self.cache, self.home, {}, False,
                                     self.now, time.monotonic()+10, proc=proc)
            self.assertTrue(hidden.exists(), 'a live process still holds its file')
        finally:
            process.terminate(); process.wait(timeout=5)
            process.stdin.close(); process.stdout.close()

    def test_an_unreadable_cwd_is_not_cleared_by_an_argument_reference(self):
        visible = self.directory('visible-evidence')
        hidden = self.directory('working-evidence')
        self.assertIsNone(self.hidden_process(
            [b'python', os.fsencode(visible)], b'python', '/usr/bin/python',
            {'cwd'}, cwd=hidden, paths=[visible, hidden]))

    def test_the_service_exemption_is_exact_and_names_what_it_skipped(self):
        import contextlib
        import io
        # Condition (a): lookalikes and near-misses never inherit the exemption.
        for arguments, comm, executable in (
                ([b'/tmp/ssh-agent'], b'ssh-agent', '/usr/bin/ssh-agent'),
                ([b'/usr/bin/ssh-agent-lookalike'], b'ssh-agent', '/usr/bin/ssh-agent'),
                ([b'/usr/lib/systemd/systemd'], b'systemd', '/usr/lib/systemd/systemd'),
                ([b'/tmp/systemd', b'--user'], b'systemd', '/usr/lib/systemd/systemd'),
                ([b'sshd-session: '], b'sshd-session', '/usr/lib/openssh/sshd-session')):
            with self.subTest(arguments=arguments):
                self.assertEqual(
                    self.hidden_process(arguments, comm, executable, {'environ'}), None)
        # Condition (c): a used exemption is reported, naming the skipped sources.
        with contextlib.redirect_stderr(io.StringIO()) as reported:
            self.hidden_process([b'/usr/bin/ssh-agent'], b'ssh-agent', '/usr/bin/ssh-agent',
                                {'environ', 'fd', 'cwd', 'exe'})
        self.assertIn('ssh-agent', reported.getvalue())
        self.assertIn('environ', reported.getvalue())
        self.assertIn('descriptors', reported.getvalue())

    def test_every_partial_visibility_pattern_stays_blind(self):
        sources = ('cwd', 'exe', 'environ', 'fd')
        for arguments, comm in (([b'ssh-agent'], b'ssh-agent'),
                                ([b'sshd-session: fixture'], b'sshd-session'),
                                ([b'python'], b'python')):
            for mask in range(1, 16):
                hidden = {name for bit, name in enumerate(sources)
                          if mask & (1 << bit)}
                with self.subTest(arguments=arguments, hidden=hidden), \
                        contextlib.redirect_stderr(io.StringIO()) as reported:
                    result = self.hidden_process(arguments, comm, '/fixture/program', hidden)
                    if mask == 15 and comm != b'python':
                        self.assertEqual(result, set())
                        self.assertIn('123', reported.getvalue())
                        for source in ('cwd', 'exe', 'environ', 'descriptors'):
                            self.assertIn(source, reported.getvalue())
                    else:
                        self.assertIsNone(result)
                        self.assertEqual(reported.getvalue(), '')

    def test_all_hidden_near_misses_never_inherit_an_exemption(self):
        hidden = {'cwd', 'exe', 'environ', 'fd'}
        for arguments, comm in (([b'/tmp/ssh-agent'], b'ssh-agent'),
                                ([b'ssh-agent-lookalike'], b'ssh-agent'),
                                ([b'ssh-agent'], b'python'),
                                ([b'/usr/lib/systemd/systemd'], b'systemd'),
                                ([b'sshd-session: '], b'sshd-session'),
                                ([b'sshd-session-lookalike: fixture'], b'sshd-session')):
            with self.subTest(arguments=arguments), \
                    contextlib.redirect_stderr(io.StringIO()) as reported:
                self.assertIsNone(self.hidden_process(arguments, comm, None, hidden))
                self.assertEqual(reported.getvalue(), '')

    def test_exempt_process_arguments_still_protect_candidates_on_every_pass(self):
        path = self.directory('referenced-evidence')
        other = self.directory('unreferenced-evidence')
        hidden = {'cwd', 'exe', 'environ', 'fd'}
        with contextlib.redirect_stderr(io.StringIO()) as reported:
            for _ in range(2):
                result = self.hidden_process([b'ssh-agent', os.fsencode(path/'job')],
                                             b'ssh-agent', None, hidden, paths=[path, other])
                self.assertEqual(result, {path})
        self.assertEqual(len(reported.getvalue().splitlines()), 2)
        for line in reported.getvalue().splitlines():
            self.assertIn('123', line)
            self.assertIn('ssh-agent', line)
            for source in ('cwd', 'exe', 'environ', 'descriptors'):
                self.assertIn(source, line)

    def test_dry_run_and_failed_cleanup_do_not_claim_deletion(self):
        path = self.directory('dry-evidence')
        self.assertEqual(self.reclaim(dry_run=True),[path]); self.assertTrue(path.exists())
        with patch.object(orphan_cache.subprocess,'run',side_effect=subprocess.TimeoutExpired('rm',1)):
            with self.assertRaises(subprocess.TimeoutExpired): self.reclaim()
        self.assertTrue(path.exists())


    def test_bare_repository_is_preserved_without_dot_git(self):
        path = self.directory('clone-20261007')
        subprocess.run(['git','init','--bare','-q',str(path)],check=True,timeout=10)
        self.age(path,7200)
        self.assertFalse((path/'.git').exists())
        self.assertEqual(self.reclaim(), [])
        self.assertTrue((path/'HEAD').exists())

    def test_open_descriptor_protects_evidence_without_cwd_or_argument_reference(self):
        path = self.directory('descriptor-evidence')
        with (path/'result.log').open() as handle:
            process = subprocess.Popen([sys.executable,'-c',
                'import os,sys,time; f=os.fdopen(int(sys.argv[1])); '
                'print("ready",flush=True); time.sleep(30)',str(handle.fileno())],
                pass_fds=(handle.fileno(),),cwd=self.home,stdout=subprocess.PIPE)
        try:
            self.assertEqual(process.stdout.readline(),b'ready\n')
            proc = self.home/'test-proc'; proc.mkdir(exist_ok=True)
            (proc/str(process.pid)).symlink_to(f'/proc/{process.pid}',target_is_directory=True)
            scanner = orphan_cache.active
            self.assertEqual(scanner([path],time.monotonic()+10,proc),{path})
            with patch.object(orphan_cache,'active',side_effect=lambda paths, deadline, proc, refused=None:
                              scanner(paths,deadline,proc)):
                self.assertEqual(orphan_cache.reclaim(self.cache,self.home,{},False,self.now,
                                                      time.monotonic()+10, proc=proc), [])
            self.assertTrue(path.exists())
        finally:
            process.terminate(); process.wait(timeout=5); process.stdout.close()
        self.assertEqual(self.reclaim(), [path])
