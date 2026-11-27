"""Offline attribution, loss, unknown usage and redaction regressions."""
import base64
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import cache_diagnostics as cache
import provider_timing
import grapt_usage


class CacheDiagnostics(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.database, self.log = self.root/'store.db', self.root/'agent.jsonl'
        self.connection = sqlite3.connect(self.database)
        self.addCleanup(self.connection.close)
        self.connection.execute('CREATE TABLE event(aggregate_family,aggregate_id,seq,type,data)')
        self.seq = 0

    def event(self, kind, data, session='ses_target'):
        self.connection.execute('INSERT INTO event VALUES (?,?,?,?,?)',
                                ('session', session, self.seq, kind, json.dumps(data)))
        self.seq += 1
        self.connection.commit()

    def opened(self, attempt='one', session='ses_target'):
        self.event('session.assistant-message.opened.1',
                   dict(attempt=attempt, step=1, message='msg_'+attempt), session)

    def usage(self, attempt='one', total=100, read=0, phase='terminal', **changes):
        counts = dict(inputTotalTokens=total, inputFreshTokens=total-read,
                      cacheReadInputTokens=read, cacheWriteInputTokens=0, outputTokens=5)
        fields = {key: dict(count=value, provenance='provider-reported') for key, value in counts.items()}
        fields.update(changes)
        self.event('session.usage.observed.2', dict(attempt=attempt, usage=dict(phase=phase, **fields)))

    def sample(self, attempt='one', change='first_observation', previous=None, session='ses_target', **extra):
        document = dict(session=session, metadata=dict(cacheRequestChange=change,
            cacheRequestChangedItem='7' if change in {'input_changed', 'input_shortened'} else None,
            cacheRequestPreviousAttempt=previous, cacheRequestBodyBytes=1500,
            cacheRequestToolsBytes=900, cacheRequestInstructionsBytes=300, **extra))
        return f'GRAPT_PROVIDER_DISPATCH attempt={attempt} at_ms=10 cache_request='+json.dumps(document)

    def write(self, *lines):
        self.log.write_text(''.join(json.dumps({'received_at': '2100-01-01',
            'raw_base64': base64.b64encode((line+'\n').encode()).decode()})+'\n' for line in lines))

    def collect(self, path=None):
        return cache.collect(self.log, self.database, 'ses_target', path)['request_cache']

    def test_exact_attempt_join_keeps_retries_summaries_and_observed_zero(self):
        self.opened()
        self.usage()
        self.event('session.content-block.opened.1',
                   dict(attempt='retry', message='msg_one', block=0, kind='text'))
        self.usage('retry', total=120, read=100)
        self.event('session.compaction.call.started.1', dict(attempt='summary'))
        self.event('session.compaction.call.usage.1', dict(attempt='summary', usage=dict(
            phase='terminal', **{key: dict(count=count, provenance='provider-reported') for key, count in
            dict(inputTotalTokens=130, inputFreshTokens=10, cacheReadInputTokens=120,
                 cacheWriteInputTokens=0, outputTokens=5).items()})))
        self.opened('foreign', 'ses_foreign')
        self.write(self.sample('summary', 'append_only', 'retry'), self.sample(),
                   self.sample('retry', 'input_changed', 'one'),
                   self.sample('foreign', session='ses_foreign'))
        report = self.collect()
        self.assertEqual(report['status'], 'complete')
        first, retry, summary = report['calls']
        self.assertEqual([row['attempt'] for row in report['calls']], ['one', 'retry', 'summary'])
        self.assertEqual(first['usage']['cacheReadInputTokens']['count'], 0)
        self.assertEqual(retry['message'], first['message'])
        self.assertEqual(retry['request']['changed_item'], 7)
        self.assertEqual(summary['request']['previous_attempt'], 'retry')
        self.assertTrue(summary['compaction_summary'])
        self.assertTrue(all(row['provider_miss_cause'] == 'unreported' for row in report['calls']))
        strict = grapt_usage.collect(self.database, 'ses_target')
        self.assertEqual(strict['usage']['cached_input_tokens'], 220)
        self.assertEqual(strict['usage']['input_tokens'], 350)

    def test_absent_usage_estimates_and_stream_only_counts_stay_unknown(self):
        self.opened()
        self.opened('two')
        self.usage('two', phase='during-stream')
        self.opened('three')
        self.usage('three', cacheReadInputTokens=dict(count=99, provenance='harness-estimated'))
        self.write(self.sample(), self.sample('two', 'append_only', 'one'),
                   self.sample('three', 'append_only', 'two'))
        report = self.collect()
        self.assertEqual(report['status'], 'partial')
        self.assertEqual([row['usage_status'] for row in report['calls']],
                         ['unavailable', 'unavailable', 'partial'])
        self.assertIsNone(report['calls'][2]['usage']['cacheReadInputTokens'])
        self.assertIsNone(grapt_usage.collect(self.database, 'ses_target')['usage'])

    def test_derived_usage_retains_its_rule_and_reconciled_phase(self):
        self.opened()
        self.usage(total=100, read=80, phase='reconciled',
                   inputFreshTokens=dict(count=20, provenance='provider-derived',
                                         rule='fresh-from-inclusive-input'))
        self.write(self.sample())
        report = self.collect()
        leaf = report['calls'][0]['usage']['inputFreshTokens']
        self.assertEqual(leaf, dict(count=20, phase='reconciled', provenance='provider-derived',
                                    rule='fresh-from-inclusive-input'))
        self.assertEqual(report['status'], 'complete')

    def test_missing_store_keeps_dispatch_evidence_with_unknown_usage(self):
        self.write(self.sample())
        self.database.unlink()
        report = self.collect()
        self.assertEqual(report['status'], 'partial')
        self.assertIn('usage_capture_error', report)
        self.assertEqual(report['calls'][0]['attribution'], 'dispatch-only')
        self.assertEqual(report['calls'][0]['request']['change'], 'first_observation')
        self.assertTrue(all(value is None for value in report['calls'][0]['usage'].values()))

    def test_old_logs_or_missing_logs_keep_usage_and_missing_request_side(self):
        self.opened(); self.usage(read=80)
        self.write('GRAPT_PROVIDER_DISPATCH attempt=one at_ms=10')
        for missing in [False, True]:
            if missing:
                self.log.unlink()
            report = self.collect()
            self.assertEqual(report['status'], 'partial')
            self.assertIsNone(report['calls'][0]['request'])
            self.assertEqual(report['calls'][0]['usage']['cacheReadInputTokens']['count'], 80)

    def test_loss_and_comparison_bound_are_explicit(self):
        self.opened(); self.usage()
        self.write(self.sample(change='comparison_bound'),
                   'GRAPT_PROVIDER_DISPATCH_LOSS count_at_least=3',
                   'GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least=7',
                   'GRAPT_PROVIDER_TAIL_LOSS acp_count_at_least=2')
        report = self.collect()
        self.assertEqual(report['status'], 'partial')
        self.assertEqual(report['diagnostic_loss_at_least'], dict(dispatch=3, output=7, acp=2))
        self.assertEqual(report['calls'][0]['request_status'], 'limited')

    def invalid_loss(self, line):
        self.write(self.sample(), line)
        report = self.collect()
        self.assertEqual(report['status'], 'partial')
        self.assertIn('request_capture_error', report)
        self.assertEqual(report['calls'][0]['usage_status'], 'observed')
        self.assertEqual(report['calls'][0]['usage']['inputTotalTokens']['count'], 100)

    def test_dispatch_loss_requires_its_own_count_field(self):
        self.opened(); self.usage()
        self.invalid_loss('GRAPT_PROVIDER_DISPATCH_LOSS acp_count_at_least=3')

    def test_every_declared_tail_loss_count_must_be_valid(self):
        self.opened(); self.usage()
        self.invalid_loss('GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least=bad acp_count_at_least=0')

    def test_loss_record_bounds_duplicates_and_wrong_record_fields_are_rejected(self):
        self.opened(); self.usage()
        for line in [
            'GRAPT_PROVIDER_DISPATCH_LOSS',
            'GRAPT_PROVIDER_TAIL_LOSS count_at_least=0',
            'GRAPT_PROVIDER_DISPATCH_LOSS count_at_least=0 count_at_least=1',
            'GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least=0 tail_count_at_least=0',
            'GRAPT_PROVIDER_TAIL_LOSS acp_count_at_least=-1 tail_count_at_least=0',
            'GRAPT_PROVIDER_TAIL_LOSS acp_count_at_least=18446744073709551616',
            'GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least=0 unknown=0',
        ]:
            with self.subTest(line=line):
                self.invalid_loss(line)
        self.write(self.sample(), 'GRAPT_PROVIDER_DISPATCH_LOSS count_at_least=0',
                   'GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least=0 acp_count_at_least=0')
        report = self.collect()
        self.assertEqual(report['status'], 'complete')
        self.assertEqual(report['diagnostic_loss_at_least'], dict(dispatch=0, output=0, acp=0))

    def test_duplicate_malformed_or_inconsistent_samples_do_not_hide_usage(self):
        self.opened(); self.usage()
        for lines in [(self.sample(), self.sample()),
                      ('GRAPT_PROVIDER_DISPATCH attempt=one at_ms=10 cache_request=PRIVATE_SENTINEL',),
                      (self.sample(change='input_changed').replace('"7"', '"99999"'),),
                      (self.sample(change='unknown'),)]:
            self.write(*lines)
            report = self.collect()
            self.assertEqual(report['status'], 'partial')
            self.assertIn('request_capture_error', report)
            self.assertEqual(report['calls'][0]['usage_status'], 'observed')
            self.assertNotIn('PRIVATE_SENTINEL', json.dumps(report))

    def test_export_projects_only_known_safe_metadata(self):
        self.opened(); self.usage()
        self.write(self.sample(credential='PRIVATE_SENTINEL', request_body='PRIVATE_SENTINEL'))
        report = self.collect(self.root/'request-cache.json')
        self.assertEqual(report['status'], 'complete')
        self.assertNotIn('PRIVATE_SENTINEL', json.dumps(report))
        self.assertNotIn('PRIVATE_SENTINEL', (self.root/'request-cache.json').read_text())
        before = (self.root/'request-cache.json').read_bytes()
        self.assertIn('artifact_error', self.collect(self.root/'request-cache.json'))
        self.assertEqual((self.root/'request-cache.json').read_bytes(), before)

    def test_unattributed_samples_prevent_complete_coverage(self):
        self.opened(); self.usage()
        self.write(self.sample(), self.sample('unopened'))
        report = self.collect()
        self.assertEqual(report['status'], 'partial')
        self.assertEqual(report['unattributed_request_samples'], 1)
        self.assertEqual(len(report['calls']), 2)
        self.assertEqual(report['calls'][1]['attribution'], 'dispatch-only')
        self.assertEqual(report['calls'][1]['usage_status'], 'unavailable')

    def test_store_corruption_partition_conflicts_and_bounds_are_unavailable(self):
        self.opened(); self.usage(read=101)
        self.write(self.sample())
        self.assertIn('usage_capture_error', self.collect())
        self.connection.execute('DELETE FROM event'); self.connection.commit()
        self.seq = 0
        self.opened(); self.usage()
        with patch.object(grapt_usage, 'MAX_ROWS', 1):
            self.assertIn('usage_capture_error', self.collect())
        with patch.object(provider_timing, 'MAX_LOG', 1):
            self.assertIn('request_capture_error', self.collect())
        with patch.object(cache, 'MAX_LINE', 1):
            self.assertIn('request_capture_error', self.collect())
        self.database.unlink()
        self.assertIn('usage_capture_error', self.collect())


if __name__ == '__main__':
    unittest.main()
