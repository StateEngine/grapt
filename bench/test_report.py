"""Reports distinguish transport completion, task checks and missing evidence."""
import unittest
import report
import json
from pathlib import Path
import tempfile


class ReportTests(unittest.TestCase):
    def test_pending_reason_and_full_denominator_survive_then_resolve_without_retry(self):
        measured = {'harness': 'codex', 'task': 'one', 'status': 'failed', 'success': False}
        first = {'corpus': 'same', 'held_out_included': True, 'planned_tasks': ['one'],
                 'planned_harnesses': ['codex', 'claude'], 'attempts': [measured],
                 'deferred_harnesses': {'claude': 'Subscription held until the stated window'}}
        text = report.markdown([first])
        self.assertIn('Measurement status: partial (1/2', text)
        self.assertIn('claude: 1/1 pending. Subscription held', text)
        later = dict(first, planned_harnesses=['claude'], deferred_harnesses={},
                     attempts=[{'harness': 'claude', 'task': 'one', 'status': 'failed', 'success': False}])
        text = report.markdown([first, later])
        self.assertIn('Measurement status: complete (2/2', text)
        self.assertNotIn('## Pending attempts', text)
        self.assertEqual(first['attempts'], [measured])
        with self.assertRaisesRegex(ValueError, 'Duplicate attempt'):
            report.markdown([first, later, later])

    def test_sonnet_and_haiku_segments_cannot_be_relabelled_as_one_comparison(self):
        original = {'corpus': 'same', 'planned_tasks': ['one'], 'planned_harnesses': ['claude'],
                    'attempts': [], 'adapters': {'claude': {'model': 'claude-sonnet-5-5',
                                                        'cohort': 'sonnet', 'auth': 'claude-max'}}}
        haiku = dict(original, adapters={'claude': {'model': 'claude-haiku-5-5',
                                                  'cohort': 'haiku', 'auth': 'claude-max'}})
        with self.assertRaisesRegex(ValueError, 'separate comparison'):
            report.combined([original, haiku])
        self.assertEqual(original['adapters']['claude']['cohort'], 'sonnet')

    def test_completed_zero_live_row_is_a_separate_streaming_defect(self):
        from run import telemetry
        records = []
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/'agent.jsonl'
            for task, kinds in [('zero', []), ('normal', [
                    'agent_message_chunk', 'tool_call', 'tool_call_update', 'agent_thought_chunk'])]:
                events = [{'method': 'session/update', 'params': {'update': {'sessionUpdate': kind}}}
                          for kind in ['available_commands_update', *kinds]]
                log.write_text('\n'.join(json.dumps({'received_at': '2026-10-07T00:00:00Z',
                    'event': event}) for event in events)+'\n')
                row = {'harness': 'grapt', 'task': task, 'status': 'completed', 'success': True}
                row.update(telemetry(log, 'grapt'))
                records.append(row)
        self.assertTrue(report.streaming_defect(records[0]))
        self.assertFalse(report.streaming_defect(records[1]))
        self.assertEqual(records[1]['live_acp_updates'], dict.fromkeys(report.LIVE_KINDS, 1))
        self.assertIsNone(report.streaming_defect(dict(records[0], live_acp_updates=None)))
        self.assertIsNone(report.streaming_defect(dict(records[0], status='failed')))
        result = report.summary(records, ['grapt'], ['zero', 'normal'])['grapt']
        self.assertEqual(result['successful_attempts'], 2)
        self.assertEqual(result['streaming_defects'], 1)
        self.assertEqual(result['streaming_defect_coverage'], 2)
        text = report.markdown([{'corpus': 'same', 'planned_tasks': ['zero', 'normal'],
            'planned_harnesses': ['grapt'], 'attempts': records}])
        self.assertIn('| grapt | zero | completed | 0 | 0 | 0 | 0 | yes |', text)
        self.assertIn('| grapt | normal | completed | 1 | 1 | 1 | 1 | no |', text)

    def records(self):
        return [
            {'harness': 'probe', 'task': 'transport', 'status': 'failed', 'success': False,
             'test_status': 'completed', 'test_exit': 0},
            {'harness': 'probe', 'task': 'task', 'status': 'completed', 'success': False,
             'test_status': 'failed', 'test_exit': 1},
            {'harness': 'probe', 'task': 'both', 'status': 'completed', 'success': True,
             'test_status': 'completed', 'test_exit': 0},
        ]

    def test_opposing_failures_remain_distinct_and_missing_rows_count(self):
        result = report.summary(self.records(), ['probe'],
                                ['transport', 'task', 'both', 'missing'])['probe']
        self.assertEqual(result['successful_attempts'], 1)
        self.assertEqual(result['native_completed'], 2)
        self.assertEqual(result['hidden_tests_passed'], 2)
        self.assertEqual(result['hidden_tests_coverage'], 3)
        self.assertEqual(result['success_rate'], .25)
        self.assertEqual(result['status_counts']['missing'], 1)

    def test_old_success_without_evaluator_evidence_is_not_a_hidden_pass(self):
        result = report.summary([{'harness': 'probe', 'task': 'old',
            'status': 'completed', 'success': True}], ['probe'], ['old'])['probe']
        self.assertEqual(result['successful_attempts'], 1)
        self.assertEqual(result['hidden_tests_passed'], 0)
        self.assertEqual(result['hidden_tests_coverage'], 0)

    def test_markdown_shows_each_dimension_and_evaluator_coverage(self):
        text = report.markdown([{'corpus': 'same', 'held_out_included': False,
            'planned_tasks': ['transport', 'task', 'both', 'missing'],
            'planned_harnesses': ['probe'], 'attempts': self.records()}])
        self.assertIn('Successful attempts | Native completed | Hidden checks passed', text)
        self.assertIn('| 1/4 | 2/4 | 2/4 (3 evaluated) |', text)
