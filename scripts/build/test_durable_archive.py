"""Successful job retention changes only at the two-hour boundary."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

import durable_archive
import durable_jobs


class SuccessfulJobRetention(unittest.TestCase):
    def test_success_waits_for_two_hours_of_both_terminal_timestamps(self):
        now = 50_000
        for finished_age, modified_age, expected in (
                (7_199, 7_199, False),
                (7_200, 7_200, True),
                (7_201, 7_201, True),
                (7_200, 7_199, False),
                (7_199, 7_200, False)):
            with self.subTest(finished_age=finished_age, modified_age=modified_age):
                with tempfile.TemporaryDirectory() as raw:
                    path = Path(raw) / ("a" * 32)
                    path.mkdir()
                    result = path / "result.json"
                    result.write_text("{}", encoding="utf-8")
                    os.utime(result, (now - modified_age, now - modified_age))
                    terminal = {"finished_unix": now - finished_age, "exit": 0}
                    with patch.object(durable_archive.jobs, "read", return_value=terminal):
                        self.assertEqual(durable_archive.eligible(path, now, {}), expected)


class RetentionBoundaries(unittest.TestCase):
    SOURCE = 'b' * 40

    def _job(self, base, token, *, exit_code, finished_unix, age, pin=False):
        (base/'jobs').mkdir(mode=0o700, exist_ok=True)
        path = base/'jobs'/token
        path.mkdir(mode=0o700)
        request = dict(job=token, unit='secode-job-'+token+'.service', owner='owner',
                       source=self.SOURCE, cwd=str(base), log=str(path/'output.log'))
        (path/'request.json').write_text(json.dumps(request), encoding='utf-8')
        result = dict(job=token, source=self.SOURCE, owner='owner', exit=exit_code,
                      finished_unix=finished_unix)
        (path/'result.json').write_text(json.dumps(result), encoding='utf-8')
        (path/'lifecycle.lock').touch(mode=0o600)
        (path/'output.log').write_bytes(b'original bytes\n')
        if pin:
            (path/'retain-for-receipt').touch()
        stamp = time.time()-age
        os.utime(path/'result.json', (stamp, stamp))
        return path

    def test_failure_at_two_hours_stays_while_current(self):
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            now = time.time()
            path = self._job(base, 'a'*32, exit_code=1, finished_unix=now-7200, age=7200)
            head = subprocess.CompletedProcess([], 0, stdout=self.SOURCE+'\n')
            with patch.object(durable_archive.subprocess, 'run', return_value=head):
                self.assertFalse(durable_archive.eligible(path, now, {}))

    def test_pinned_success_stays_past_two_hours(self):
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            now = time.time()
            path = self._job(base, 'a'*32, exit_code=0, finished_unix=now-10800, age=10800, pin=True)
            self.assertFalse(durable_archive.eligible(path, now, {}))

    def test_missing_or_nonfinite_finished_unix_is_preserved(self):
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            now = time.time()
            for index, finished in enumerate((None, float('inf'))):
                with self.subTest(finished=finished):
                    token = 'c'*31+str(index)
                    path = self._job(base, token, exit_code=0, finished_unix=now-10800, age=10800)
                    result = json.loads((path/'result.json').read_text(encoding='utf-8'))
                    if finished is None:
                        del result['finished_unix']
                    else:
                        result['finished_unix'] = finished
                    (path/'result.json').write_text(json.dumps(result), encoding='utf-8')
                    with self.assertRaisesRegex(ValueError, 'Invalid terminal result'):
                        durable_archive.eligible(path, now, {})

    def test_future_timestamp_is_preserved(self):
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            now = time.time()
            for index, exit_code in enumerate((0, 1)):
                with self.subTest(exit_code=exit_code):
                    token = 'd'*31+str(index)
                    path = self._job(base, token, exit_code=exit_code, finished_unix=now+3600, age=0)
                    self.assertFalse(durable_archive.eligible(path, now, {}))

    def test_a_nonobject_result_does_not_block_other_eligible_jobs(self):
        for malformed in ([], None):
            with self.subTest(result=malformed), tempfile.TemporaryDirectory() as raw:
                base = Path(raw)
                now = time.time()
                broken = self._job(base, 'a'*32, exit_code=0,
                                   finished_unix=now-10800, age=10800)
                eligible = self._job(base, 'b'*32, exit_code=0,
                                     finished_unix=now-7201, age=7201)
                result = broken/'result.json'
                original = json.dumps(malformed).encode()
                result.write_bytes(original)
                os.utime(result, (now-10800,)*2)
                with (patch.object(durable_jobs, 'JOBS', base/'jobs'),
                      patch.object(durable_jobs, 'service_state', return_value='inactive')):
                    with self.assertRaisesRegex(ValueError, 'matching terminal result'):
                        durable_archive.archive(broken.name)
                    row = durable_archive.sweep(apply=True, actor='agt-test-actor')
                self.assertEqual(row['selected'], [eligible.name])
                self.assertTrue(any(broken.name in error for error in row['errors']))
                self.assertEqual(result.read_bytes(), original)
                self.assertFalse(eligible.exists())

    def test_sweep_moves_bytes_intact_and_refuses_an_existing_destination(self):
        with tempfile.TemporaryDirectory() as raw:
            base = Path(raw)
            jobs = base/'jobs'
            jobs.mkdir(mode=0o700)
            now = time.time()
            moved = 'a'*32
            held = 'c'*32
            for token in (moved, held):
                path = jobs/token
                path.mkdir(mode=0o700)
                request = dict(job=token, unit='secode-job-'+token+'.service', owner='owner',
                               source=self.SOURCE, cwd=str(base), log=str(path/'output.log'))
                (path/'request.json').write_text(json.dumps(request), encoding='utf-8')
                (path/'result.json').write_text(json.dumps(dict(job=token, source=self.SOURCE,
                    owner='owner', exit=0, finished_unix=now-7201)), encoding='utf-8')
                (path/'lifecycle.lock').touch(mode=0o600)
                (path/'output.log').write_bytes(('job '+token[:4]+' bytes\n').encode())
                os.utime(path/'result.json', (now-7201,)*2)
            originals = {token: {name: (jobs/token/name).read_bytes() for name in
                         ('request.json', 'result.json', 'output.log')} for token in (moved, held)}
            archive_dir = base/'completed-jobs'
            archive_dir.mkdir(mode=0o700)
            (archive_dir/held).mkdir(mode=0o700)
            with (patch.object(durable_jobs, 'JOBS', jobs),
                  patch.object(durable_jobs, 'service_state', return_value='inactive')):
                with self.assertRaises(FileExistsError):
                    durable_archive.archive(held)
                row = durable_archive.sweep(apply=True, actor='agt-test-actor')
            self.assertEqual(row['selected'], [moved])
            self.assertFalse((jobs/moved).exists())
            destination = base/'completed-jobs'/moved
            self.assertEqual({name: (destination/name).read_bytes() for name in originals[moved]},
                             originals[moved])
            self.assertTrue((jobs/held).exists())
            self.assertEqual({name: (jobs/held/name).read_bytes() for name in originals[held]},
                             originals[held])
            self.assertTrue(any('Preserve the existing archived job' in error for error in row['errors']))
