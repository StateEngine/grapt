"""Input efficiency needs real call, partition and billing evidence."""
import unittest
import cache_metrics
import json
from pathlib import Path
import tempfile
import run


class CacheMetricsTests(unittest.TestCase):
    def row(self):
        return {'status': 'completed', 'success': False,
                'usage': {'input_tokens': 1000, 'cached_input_tokens': 600, 'output_tokens': 42},
                'cache_write_tokens_observed': 100,
                'cache_write_durations': {'5m': 0, '1h': 100},
                'call_usage': [{'identity': 'first', 'input_tokens': 200, 'cached_input_tokens': 100},
                               {'identity': 'second', 'input_tokens': 800, 'cached_input_tokens': 500}]}

    def test_writes_are_partitioned_and_actual_duration_is_priced(self):
        row = self.row()
        metric = cache_metrics.row_metrics(row, 'anthropic/claude-haiku-5.5', cache_metrics.prices())
        self.assertEqual(metric['uncached_input_tokens'], 300)
        self.assertEqual(metric['uncached_input_per_call'], 150)
        self.assertEqual(metric['first_call_cached_share'], .5)
        self.assertEqual(metric['input_equivalent'], 300+60+200)
        row['cache_write_durations'] = {'5m': 100, '1h': 0}
        metric = cache_metrics.row_metrics(row, 'anthropic/claude-haiku-5.5', cache_metrics.prices())
        self.assertEqual(metric['input_equivalent'], 300+60+125)

    def test_missing_duration_or_write_count_is_unknown_not_simple_proxy(self):
        row = self.row()
        del row['cache_write_durations']
        metric = cache_metrics.row_metrics(row, 'anthropic/claude-haiku-5.5', cache_metrics.prices())
        self.assertIsNone(metric['input_equivalent'])
        del row['cache_write_tokens_observed']
        metric = cache_metrics.row_metrics(row, 'anthropic/claude-haiku-5.5', cache_metrics.prices())
        self.assertIsNone(metric['uncached_input_tokens'])
        self.assertIsNone(metric['input_equivalent'])

    def test_openai_has_no_write_premium_but_needs_reported_count(self):
        row = self.row()
        metric = cache_metrics.row_metrics(row, 'gpt-6-luna', cache_metrics.prices())
        self.assertEqual(metric['input_equivalent'], 360)
        del row['cache_write_tokens_observed']
        self.assertIsNone(cache_metrics.row_metrics(row, 'gpt-6-luna', cache_metrics.prices())['input_equivalent'])

    def test_completed_coding_failure_counts_but_missing_usage_is_not_zero(self):
        failed = self.row()
        crashed = dict(failed, status='crash')
        summary = cache_metrics.summarize([failed, crashed], 'gpt-6-luna')
        self.assertEqual(summary['native_completed'], 1)
        self.assertEqual(summary['uncached_input_per_completed_run'], 300)
        summary = cache_metrics.summarize([failed, dict(failed, usage=None)], 'gpt-6-luna')
        self.assertIsNone(summary['uncached_input_per_completed_run'])
        self.assertEqual(summary['uncached_coverage_runs'], 1)

    def test_whole_run_usage_cannot_establish_first_call_or_call_count(self):
        row = self.row()
        del row['call_usage']
        metric = cache_metrics.row_metrics(row, 'gpt-6-luna', cache_metrics.prices())
        self.assertIsNone(metric['calls'])
        self.assertIsNone(metric['first_call_cached_share'])
        self.assertIsNone(metric['uncached_input_per_call'])

    def test_native_claude_chunks_count_each_message_once(self):
        calls = cache_metrics.Calls('claude')
        event = {'type': 'assistant', 'message': {'id': 'message-one', 'usage': {
            'input_tokens': 5, 'cache_read_input_tokens': 10, 'cache_creation_input_tokens': 2,
            'cache_creation': {'ephemeral_5m_input_tokens': 2, 'ephemeral_1h_input_tokens': 0}}}}
        calls.observe(event)
        calls.observe(event)
        result = calls.result({'input_tokens': 17, 'cached_input_tokens': 10})
        self.assertEqual(len(result), 1)
        self.assertEqual(result[0]['cache_write_durations'], {'5m': 2, '1h': 0})
        event['message']['usage']['input_tokens'] = 6
        calls.observe(event)
        self.assertIsNone(calls.result({'input_tokens': 17, 'cached_input_tokens': 10}))

    def test_incomplete_or_excessive_call_evidence_is_refused(self):
        calls = cache_metrics.Calls('opencode')
        event = {'type': 'step_finish', 'part': {'messageID': 'm1', 'tokens': {
            'input': 10, 'cache': {'read': 5, 'write': 0}}}}
        calls.observe(event)
        self.assertIsNone(calls.result({'input_tokens': 30, 'cached_input_tokens': 10}))
        del event['part']['tokens']['cache']['write']
        calls.observe(event)
        self.assertIsNone(calls.result({'input_tokens': 15, 'cached_input_tokens': 5}))

    def test_only_provider_reported_cost_is_billed(self):
        row = self.row()
        row['provider_billing'] = {'usd': '0.012345', 'provenance': 'provider-reported'}
        self.assertEqual(cache_metrics.row_metrics(row, 'gpt-6-luna', cache_metrics.prices())['provider_billed_usd'], .012345)
        row['provider_billing']['provenance'] = 'harness-estimated'
        self.assertIsNone(cache_metrics.row_metrics(row, 'gpt-6-luna', cache_metrics.prices())['provider_billed_usd'])

    def test_codex_whole_turn_preserves_totals_without_fabricating_calls(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'agent.jsonl'
            log.write_text(json.dumps({'type': 'turn.completed', 'usage': {
                'input_tokens': 100, 'cached_input_tokens': 40, 'output_tokens': 2,
                'cache_write_input_tokens': 0}})+'\n')
            value = run.telemetry(log, 'codex')
        self.assertEqual(value['usage']['input_tokens'], 100)
        self.assertEqual(value['cache_write_tokens_observed'], 0)
        self.assertIsNone(value['call_usage'])

    def test_native_opencode_call_order_and_totals_are_reconciled(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'agent.jsonl'
            events = [{'type': 'step_finish', 'part': {'messageID': name, 'tokens': {
                'input': fresh, 'output': 2, 'cache': {'read': read, 'write': 0}}}}
                for name, fresh, read in [('first', 10, 0), ('second', 20, 100)]]
            log.write_text('\n'.join(json.dumps(e) for e in events)+'\n')
            value = run.telemetry(log, 'opencode')
        self.assertEqual([c['identity'] for c in value['call_usage']], ['first', 'second'])
        self.assertEqual(value['usage'], {'input_tokens': 130, 'cached_input_tokens': 100, 'output_tokens': 4})
        self.assertEqual(value['cache_write_tokens_observed'], 0)

    def test_malformed_calls_and_bound_stay_unknown(self):
        calls = cache_metrics.Calls('opencode')
        calls.observe({'type': 'step_finish', 'part': None})
        self.assertIsNone(calls.result({'input_tokens': 0, 'cached_input_tokens': 0}))
        calls = cache_metrics.Calls('opencode')
        for index in range(cache_metrics.MAX_CALLS+1):
            calls.observe({'type': 'step_finish', 'part': {'messageID': str(index),
                'tokens': {'input': 1, 'cache': {'read': 0, 'write': 0}}}})
        self.assertIsNone(calls.result({'input_tokens': cache_metrics.MAX_CALLS+1, 'cached_input_tokens': 0}))

    def test_missing_native_write_field_overrides_legacy_default_zero(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'agent.jsonl'
            log.write_text(json.dumps({'type': 'result', 'usage': {
                'input_tokens': 10, 'cache_read_input_tokens': 5, 'output_tokens': 2}})+'\n')
            row = run.telemetry(log, 'claude')
        self.assertEqual(row['cache_write_tokens'], 0)  # Preserve the old aggregate field.
        self.assertIsNone(row['cache_write_tokens_observed'])
        value = cache_metrics.row_metrics(row, 'claude-haiku-5-5', cache_metrics.prices())
        self.assertIsNone(value['input_equivalent'])
        self.assertIsNone(value['uncached_input_tokens'])

    def test_billed_cost_does_not_depend_on_token_coverage(self):
        row = {'status': 'completed', 'usage': None,
               'provider_billing': {'usd': '0.3', 'provenance': 'provider-reported'}}
        value = cache_metrics.summarize([row], 'gpt-6-luna')
        self.assertEqual(value['provider_billed_usd_total'], .3)
        self.assertEqual(value['billed_coverage_runs'], 1)
        self.assertIsNone(value['input_equivalent_per_completed_run'])

    def test_direct_legacy_report_cannot_promote_default_write_zero(self):
        row = {'status': 'completed', 'usage': {
            'input_tokens': 15, 'cached_input_tokens': 5, 'output_tokens': 2},
            'cache_write_tokens': 0}
        value = cache_metrics.row_metrics(row, 'claude-haiku-5-5', cache_metrics.prices())
        self.assertIsNone(value['uncached_input_tokens'])
        self.assertIsNone(value['input_equivalent'])
        summary = cache_metrics.summarize([row], 'claude-haiku-5-5')
        self.assertEqual(summary['uncached_coverage_runs'], 0)
        self.assertEqual(summary['price_coverage_runs'], 0)

    def test_conflicting_native_write_partition_cannot_override_aggregate(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'agent.jsonl'
            events = [
                {'type': 'assistant', 'message': {'id': 'm1', 'usage': {
                    'input_tokens': 60, 'cache_read_input_tokens': 40,
                    'cache_creation_input_tokens': 0}}},
                {'type': 'result', 'usage': {'input_tokens': 50, 'cache_read_input_tokens': 40,
                    'cache_creation_input_tokens': 10, 'output_tokens': 2}}]
            log.write_text('\n'.join(json.dumps(e) for e in events)+'\n')
            row = run.telemetry(log, 'claude')
        self.assertEqual(row['usage'], {'input_tokens': 100, 'cached_input_tokens': 40, 'output_tokens': 2})
        self.assertEqual(row['cache_write_tokens'], 10)  # Keep the original aggregate intact.
        self.assertIsNone(row['call_usage'])
        self.assertIsNone(row['cache_write_tokens_observed'])
        metric = cache_metrics.row_metrics(row, 'claude-haiku-5-5', cache_metrics.prices())
        self.assertIsNone(metric['uncached_input_tokens'])
        self.assertIsNone(metric['input_equivalent'])
        self.assertIsNone(metric['uncached_input_per_call'])
