"""Replay cannot overwrite measured outcomes or silently change old totals."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

import replay_cache
import report


class ReplayTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        log = self.root/'opencode'/'one'/'agent.jsonl'
        log.parent.mkdir(parents=True)
        log.write_text(json.dumps({'type': 'step_finish', 'part': {
            'messageID': 'first', 'tokens': {'input': 10, 'output': 2,
                                          'cache': {'read': 5, 'write': 0}}}})+'\n')
        row = {'harness': 'opencode', 'task': 'one', 'status': 'completed', 'success': False,
               'usage': {'input_tokens': 15, 'cached_input_tokens': 5, 'output_tokens': 2},
               'requested_model': 'openai/gpt-6-luna', 'cache_write_tokens': 0}
        summary = report.summary([row], ['opencode'], ['one'])
        summary['opencode'].pop('cache_metrics')
        self.original = {'attempts': [row], 'planned_harnesses': ['opencode'],
                         'planned_tasks': ['one'], 'summary': summary}

    def test_original_fields_and_totals_survive_enrichment(self):
        before = copy.deepcopy(self.original)
        replay = replay_cache.enrich(self.original, self.root)
        self.assertEqual(self.original, before)
        for key, value in before['attempts'][0].items():
            self.assertEqual(replay['attempts'][0][key], value)
        for key, value in before['summary']['opencode'].items():
            self.assertEqual(replay['summary']['opencode'][key], value)
        self.assertEqual(replay['summary']['opencode']['cache_metrics'][0]['uncached_input_per_call'], 10)
        self.assertFalse(replay['attempts'][0]['success'])

    def test_changed_native_usage_or_saved_summary_refuses(self):
        self.original['attempts'][0]['usage']['input_tokens'] = 16
        with self.assertRaisesRegex(ValueError, 'Native usage differs'):
            replay_cache.enrich(self.original, self.root)
        self.original['attempts'][0]['usage']['input_tokens'] = 15
        self.original['summary']['opencode']['successful_attempts'] = 1
        with self.assertRaisesRegex(ValueError, 'Original summary changed'):
            replay_cache.enrich(self.original, self.root)
