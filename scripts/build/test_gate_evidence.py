import hashlib
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import gate_evidence as evidence


class DependencyEvidenceTests(unittest.TestCase):
    def test_installed_controls_include_failure_evidence_dependency(self):
        import install_schedules
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            controls = root/'controls'
            install_schedules.install_control(Path(__file__).resolve().parents[2], controls)
            subprocess.run([sys.executable, '-B', '-c', 'import gate; import gate_evidence; import ram_targets'],
                           cwd=root, env=dict(os.environ, PYTHONPATH=str(controls)),
                           check=True, capture_output=True, timeout=10)

    def test_hashes_survive_target_removal_without_following_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / 'target'
            deps = target / 'debug/deps'
            deps.mkdir(parents=True)
            (deps / 'libreqwest-123.rlib').write_bytes(b'compiled library')
            (deps / 'librustls-456.rmeta').write_bytes(b'compiler metadata')
            secret = root / 'outside'
            secret.write_bytes(b'not a dependency')
            (deps / 'liblinked-789.rlib').symlink_to(secret)
            log = root / 'evidence/gate.log'
            evidence.preserve(target, log)
            shutil.rmtree(target)
            saved = json.loads(log.with_suffix('.log.deps.json').read_text())
            files = {row['name']: row for row in saved['files']}
            self.assertEqual(files['libreqwest-123.rlib']['sha256'],
                             hashlib.sha256(b'compiled library').hexdigest())
            self.assertEqual(files['librustls-456.rmeta']['sha256'],
                             hashlib.sha256(b'compiler metadata').hexdigest())
            self.assertNotIn('sha256', files['liblinked-789.rlib'])
            self.assertIn('error', files['liblinked-789.rlib'])

    def test_hash_limit_and_missing_directory_are_explicit(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory)
            absent = evidence.inventory(target)
            self.assertEqual(absent['incomplete'], ['dependency directory absent'])
            deps = target / 'debug/deps'
            deps.mkdir(parents=True)
            (deps / 'liblarge-123.rlib').write_bytes(b'longer than the allowance')
            with patch.object(evidence, 'MAX_BYTES', 3):
                limited = evidence.inventory(target)
            self.assertTrue(limited['incomplete'])
            self.assertNotIn('sha256', limited['files'][0])
            self.assertEqual(limited['files'][0]['error'], 'hash byte/time limit')

    def test_unwritable_evidence_does_not_replace_the_gate_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            blocked = root / 'blocked'
            blocked.write_text('a file, not a directory')
            with patch('sys.stderr') as stderr:
                evidence.preserve(root / 'target', blocked / 'gate.log')
            self.assertIn('evidence unavailable', ''.join(call.args[0] for call in stderr.write.call_args_list))

    def test_disk_gate_failure_preserves_dependency_inventory(self):
        import contextlib
        import io
        import gate
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root/'target'
            deps = target/'debug/deps'
            deps.mkdir(parents=True)
            (deps/'librustls-123.rmeta').write_bytes(b'disk metadata')
            log = root/'evidence/failed.log'
            args = gate.parse_args(['affected', '--base', 'base', '--log', str(log)])
            held = (root/'lease').open('w')
            def output(*command):
                if command[:2] == ('cargo', 'metadata'):
                    return b'{"packages":[],"workspace_members":[]}'
                return str(root).encode()
            with patch.object(gate, 'parse_args', return_value=args), \
                    patch.object(gate, 'output', side_effect=output), \
                    patch.object(gate, 'changed_paths', return_value={'docs/guide.md'}), \
                    patch.object(gate.publication_scan, 'check', return_value=[]), \
                    patch.object(gate.evict_targets, 'prepare', return_value=held), \
                    patch.object(gate.evict_targets, 'inherit'), \
                    patch.object(gate, 'claim_target'), \
                    patch.object(gate.receipt, 'source', return_value={}), \
                    patch.object(gate.signal, 'signal'), \
                    patch.object(gate, 'execute', side_effect=subprocess.CalledProcessError(101, ['cargo','test'])), \
                    patch.dict(os.environ, {'CARGO_TARGET_DIR':str(target), 'SECODE_TARGET_ROOT':str(root),
                                   'SECODE_GATE_FAILURE_ARCHIVE':str(root/'archive')}, clear=True), \
                    contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(gate.main(), 1)
            saved = json.loads(log.with_suffix('.log.deps.json').read_text())
            self.assertEqual(saved['files'][0]['sha256'], hashlib.sha256(b'disk metadata').hexdigest())
            groups = list((root/'archive').iterdir())
            self.assertEqual(len(groups), 1)
            self.assertEqual((groups[0] / 'failed.log').read_bytes(), log.read_bytes())
            self.assertEqual(json.loads((groups[0] / 'failed.log.deps.json').read_text())['files'][0]['sha256'],
                             hashlib.sha256(b'disk metadata').hexdigest())

class FailedGateArchiveTests(unittest.TestCase):
    def make_group(self, root, name, size):
        group = root / name
        group.mkdir(parents=True)
        (group / 'gate.log').write_bytes(b'x' * size)
        (group / evidence.ARCHIVE_MARKER).write_bytes(evidence.ARCHIVE_MARKER_CONTENT)
        return group

    def stamp(self, days_ago):
        return (datetime.now(timezone.utc) - timedelta(days=days_ago)).strftime('%Y%m%dT%H%M%SZ')

    def test_archive_copies_log_and_sidecars_into_a_stamped_group(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / 'archive'
            log = root / 'gate.log'
            log.write_text('boom')
            log.with_suffix('.log.deps.json').write_text('{}')
            log.with_suffix('.log.nextest.xml').write_text('<tests/>')
            log.with_suffix('.log.ram.json').write_text('{}')
            (root / 'other.log').write_text('untouched')
            with patch.dict(os.environ, {'SECODE_GATE_FAILURE_ARCHIVE': str(archive)}, clear=True):
                evidence.archive_failure(log)
            groups = list(archive.iterdir())
            self.assertEqual(len(groups), 1)
            group = groups[0]
            self.assertRegex(group.name, r'^\d{8}T\d{6}Z-\d+$')
            self.assertEqual(sorted(p.name for p in group.iterdir()),
                             [evidence.ARCHIVE_MARKER, 'gate.log', 'gate.log.deps.json',
                              'gate.log.nextest.xml', 'gate.log.ram.json'])
            # Immediate consumers keep reading the originals.
            self.assertEqual(log.read_text(), 'boom')
            self.assertEqual(log.with_suffix('.log.deps.json').read_text(), '{}')
            self.assertEqual((root / 'other.log').read_text(), 'untouched')

    def test_prune_drops_groups_older_than_seven_days(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stale = self.make_group(root, self.stamp(8) + '-101', 8)
            fresh = self.make_group(root, self.stamp(2) + '-102', 8)
            (root / 'not-a-stamp').mkdir()
            (root / 'not-a-stamp' / 'gate.log').write_bytes(b'y')
            evidence.prune_archive(root)
            self.assertFalse(stale.is_dir())
            self.assertTrue(fresh.is_dir())
            self.assertTrue((root / 'not-a-stamp').is_dir())

    def test_prune_enforces_the_byte_bound_oldest_first(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            oldest = self.make_group(root, self.stamp(2) + '-201', 1000)
            middle = self.make_group(root, self.stamp(1) + '-202', 1000)
            newest = self.make_group(root, self.stamp(0) + '-203', 1000)
            with patch.object(evidence, 'ARCHIVE_MAX_BYTES', 2500):
                evidence.prune_archive(root)
            self.assertFalse(oldest.is_dir())
            self.assertTrue(middle.is_dir())
            self.assertTrue(newest.is_dir())

    def test_retention_error_is_visible_and_never_raises(self):
        import contextlib
        import io
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            blocker = root / 'blocker'
            blocker.write_text('file, not a directory')
            log = root / 'gate.log'
            log.write_text('boom')
            stderr = io.StringIO()
            with patch.dict(os.environ, {'SECODE_GATE_FAILURE_ARCHIVE': str(blocker / 'gate-failures')}, clear=True), \
                    contextlib.redirect_stderr(stderr):
                evidence.archive_failure(log)
            self.assertTrue(log.is_file())
            self.assertIn('retention unavailable', stderr.getvalue())

    def test_missing_log_is_a_noop(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            evidence.archive_failure(root / 'absent.log')
            self.assertEqual(list(root.iterdir()), [])
