"""Offline timing collection, unknown phases and old-reader compatibility."""
import base64
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest

import provider_timing
import run


class TimingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.log = self.root/'agent.jsonl'
        self.database = self.root/'database.sqlite3'
        connection = sqlite3.connect(self.database)
        connection.execute('CREATE TABLE event(aggregate_family, aggregate_id, seq, created, type, data)')
        connection.executemany('INSERT INTO event VALUES (?, ?, ?, ?, ?, ?)', [
            ('session', 'ses_target', 1, 100, 'session.assistant-message.opened.2', '{"attempt":"one"}'),
            ('session', 'ses_target', 2, 500, 'session.compaction.call.started.1', '{"attempt":"two"}'),
            ('session', 'ses_other', 3, 0, 'session.assistant-message.opened.2', '{"attempt":"other"}')])
        connection.commit()
        connection.close()

    def write(self, *lines):
        # received_at deliberately disagrees with embedded stamps: buffered
        # stderr delivery must never substitute for a transport observation.
        self.log.write_text(''.join(json.dumps({'received_at': '2100-01-01T00:00:00Z',
            'raw_base64': base64.b64encode((line+'\n').encode()).decode()})+'\n' for line in lines))

    def collect(self):
        return provider_timing.collect(self.log, self.database, 'ses_target')['provider_timing']

    def test_observed_head_body_and_preparation_are_separate(self):
        self.write('GRAPT_PROVIDER_DISPATCH attempt=one at_ms=200 response_head_at_ms=300 first_stream_byte_at_ms=900 http_status=200',
                   'GRAPT_PROVIDER_DISPATCH attempt=two at_ms=500 response_head_at_ms=500 first_stream_byte_at_ms=500 http_status=200',
                   'GRAPT_PROVIDER_DISPATCH attempt=other at_ms=1 response_head_at_ms=2 first_stream_byte_at_ms=3')
        result = self.collect()
        first, second = result['calls']
        self.assertEqual(first['dispatch_prep_ms'], 100)
        self.assertEqual(first['response_head_wait_ms'], 100)
        self.assertEqual(first['provider_ttfb_ms'], 700)
        self.assertEqual(first['head_to_first_stream_byte_ms'], 600)
        self.assertEqual(second['provider_ttfb_ms'], 0)
        self.assertEqual(result['dispatch_samples'], 2)
        self.assertEqual(result['ttfb_samples'], 2)

    def test_old_line_absent_line_and_null_phases_stay_unknown(self):
        for line in ['GRAPT_PROVIDER_DISPATCH attempt=one at_ms=200',
                     'GRAPT_PROVIDER_DISPATCH attempt=one at_ms=200 response_head_at_ms=null first_stream_byte_at_ms=null http_status=null']:
            self.write(line)
            first, absent = self.collect()['calls']
            self.assertEqual(first['dispatch_prep_ms'], 100)
            self.assertIsNone(first['provider_ttfb_ms'])
            self.assertIsNone(absent['dispatch_prep_ms'])
            self.assertIsNone(absent['provider_ttfb_ms'])

    def test_inverted_clock_keeps_raw_stamps_but_refuses_duration(self):
        self.write('GRAPT_PROVIDER_DISPATCH attempt=one at_ms=200 response_head_at_ms=199 first_stream_byte_at_ms=201')
        first = self.collect()['calls'][0]
        self.assertFalse(first['transport_stamps_ordered'])
        self.assertEqual(first['response_head_at_ms'], 199)
        self.assertIsNone(first['provider_ttfb_ms'])

    def test_duplicates_bad_fields_and_missing_store_are_unavailable(self):
        line = 'GRAPT_PROVIDER_DISPATCH attempt=one at_ms=200'
        self.write(line, line)
        self.assertEqual(self.collect()['status'], 'unavailable')
        self.write(line+' response_head_at_ms=oops')
        self.assertEqual(self.collect()['status'], 'unavailable')
        self.database.unlink()
        self.assertEqual(self.collect()['status'], 'unavailable')

    def test_loss_is_explicit_and_refusal_has_no_stream_byte(self):
        self.write('GRAPT_PROVIDER_DISPATCH attempt=one at_ms=200 response_head_at_ms=300 first_stream_byte_at_ms=null http_status=429',
                   'GRAPT_PROVIDER_DISPATCH_LOSS count_at_least=7')
        result = self.collect()
        self.assertEqual(result['diagnostic_loss_at_least'], 7)
        self.assertEqual(result['calls'][0]['http_status'], 429)
        self.assertIsNone(result['calls'][0]['provider_ttfb_ms'])

    def test_tail_loss_does_not_invalidate_provider_dispatch_samples(self):
        dispatch = 'GRAPT_PROVIDER_DISPATCH attempt=one at_ms=200'
        self.write(dispatch)
        baseline = self.collect()
        for loss in ['GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least=7',
                     'GRAPT_PROVIDER_TAIL_LOSS acp_count_at_least=7']:
            self.write(dispatch, loss)
            self.assertEqual(self.collect(), baseline)

    def test_current_telemetry_tolerates_optional_tail(self):
        self.write('GRAPT_PROVIDER_DISPATCH attempt=one at_ms=200')
        old = run.telemetry(self.log, 'grapt')
        self.write('GRAPT_PROVIDER_DISPATCH attempt=one at_ms=200 response_head_at_ms=300 first_stream_byte_at_ms=900 http_status=200 future_field=ignored')
        self.assertEqual(run.telemetry(self.log, 'grapt'), old)

    def test_bounded_sample_census_and_signed_clock(self):
        self.write('GRAPT_PROVIDER_DISPATCH attempt=one at_ms=-1 response_head_at_ms=0 first_stream_byte_at_ms=1')
        self.assertEqual(self.collect()['calls'][0]['provider_ttfb_ms'], 2)
        self.write(*(f'GRAPT_PROVIDER_DISPATCH attempt=a{i} at_ms=1' for i in range(provider_timing.MAX_CALLS+1)))
        self.assertEqual(self.collect()['status'], 'unavailable')


if __name__ == '__main__':
    unittest.main()
