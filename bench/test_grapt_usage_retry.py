"""No-model checks for attribution of pre-visibility retry usage."""
import copy
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import grapt_usage


class RetryUsageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.database = Path(temporary.name)/'usage.db'
        self.connection = sqlite3.connect(self.database)
        self.addCleanup(self.connection.close)
        self.connection.execute('CREATE TABLE event(aggregate_family,aggregate_id,seq,type,data)')
        self.seq = 0

    def event(self, kind, body):
        self.connection.execute("INSERT INTO event VALUES('session','ses_retry',?,?,?)",
                                (self.seq, kind, json.dumps(body)))
        self.seq += 1
        self.connection.commit()

    def opened(self, attempt='first', message='msg_first'):
        self.event('session.assistant-message.opened.1',
                   dict(attempt=attempt, message=message, step=1))

    def block(self, attempt='retry', message='msg_first', block=0):
        self.event('session.content-block.opened.1',
                   dict(attempt=attempt, message=message, block=block, kind='text'))

    def tool(self):
        return dict(message='msg_first', item=dict(
            ordinal=0, kind='tool', call='outer', disposition='complete',
            payload=dict(id='art_tool', length=2, digest='digest_tool')))

    def usage(self, attempt, total, read):
        counts = dict(inputTotalTokens=total, inputFreshTokens=total-read,
                      outputTokens=2, cacheReadInputTokens=read, cacheWriteInputTokens=0)
        fields = {key: dict(count=value, provenance='provider-reported')
                  for key, value in counts.items()}
        self.event('session.usage.observed.2',
                   dict(attempt=attempt, usage=dict(phase='terminal', **fields)))

    def collect(self):
        return grapt_usage.collect(self.database, 'ses_retry')

    def test_text_retry_usage_is_charged_once_alongside_initial_attempt(self):
        self.opened()
        self.usage('first', 30, 20)
        self.block()
        self.usage('retry', 70, 50)
        result = self.collect()
        self.assertEqual(result['usage'],
                         dict(input_tokens=100, cached_input_tokens=70, output_tokens=4))
        self.assertEqual(result['grapt_usage_evidence']['attempts'], 2)

    def test_retry_missing_initial_usage_stays_unknown(self):
        self.opened()
        self.block()
        self.usage('retry', 70, 50)
        self.assertIsNone(self.collect()['usage'])

    def test_orphan_retry_usage_is_not_admitted(self):
        self.opened()
        self.usage('first', 30, 20)
        self.block(message='msg_foreign')
        self.usage('retry', 70, 50)
        self.assertIsNone(self.collect()['usage'])

    def test_attempt_cannot_bind_to_two_assistant_messages(self):
        self.opened()
        self.usage('first', 30, 20)
        self.opened('other', 'msg_second')
        self.usage('other', 10, 0)
        self.block()
        self.block(message='msg_second')
        self.usage('retry', 70, 50)
        self.assertIsNone(self.collect()['usage'])

    def test_tool_only_retry_can_bind_after_its_usage(self):
        self.opened()
        self.usage('first', 30, 20)
        self.event('session.content.appended.1',
                   self.tool())
        self.usage('retry', 70, 50)
        self.event('session.tool.terminal.1',
                   dict(attempt='retry', call='outer', outcome=dict(kind='completed')))
        self.assertEqual(self.collect()['usage'],
                         dict(input_tokens=100, cached_input_tokens=70, output_tokens=4))

    def test_nested_tool_terminal_is_not_retry_binding_evidence(self):
        self.opened()
        self.usage('first', 30, 20)
        self.usage('nested', 70, 50)
        self.event('session.tool.terminal.1',
                   dict(attempt='nested', call='child', outcome=dict(kind='completed')))
        self.assertIsNone(self.collect()['usage'])

    def test_compaction_attempt_cannot_bind_to_an_assistant(self):
        self.opened()
        self.usage('first', 30, 20)
        self.event('session.compaction.call.started.1', dict(attempt='summary'))
        self.block(attempt='summary')
        self.assertIsNone(self.collect()['usage'])

    def test_multiple_blocks_and_repeated_usage_do_not_charge_retry_twice(self):
        self.opened()
        self.usage('first', 30, 20)
        self.block()
        self.usage('retry', 70, 50)
        self.block(block=1)
        self.usage('retry', 70, 50)
        self.assertEqual(self.collect()['usage']['input_tokens'], 100)

    def test_retry_enumeration_still_obeys_the_attempt_bound(self):
        self.opened()
        self.usage('first', 30, 20)
        self.block()
        self.usage('retry', 70, 50)
        with patch.object(grapt_usage, 'MAX_ATTEMPTS', 1):
            self.assertIsNone(self.collect()['usage'])

    def test_malformed_binding_fields_stay_unknown(self):
        self.opened()
        self.usage('first', 30, 20)
        malformed = [
            ('session.content-block.opened.1',
             dict(attempt='retry', message=[], block=0, kind='text')),
            ('session.content-block.opened.1',
             dict(attempt='retry', message='msg_first', block=0, kind=[])),
            ('session.tool.terminal.1',
             dict(attempt='retry', call=[], outcome=dict(kind='completed'))),
            ('session.content.appended.1',
             mutated(self.tool(), ('message',), [])),
            ('session.content.appended.1',
             mutated(self.tool(), ('item', 'kind'), [])),
            ('session.content-block.opened.1', []),
        ]
        for kind, body in malformed:
            with self.subTest(kind=kind, body=body):
                self.connection.execute('DELETE FROM event WHERE seq>=2')
                self.connection.commit()
                self.event(kind, body)
                self.assertIsNone(self.collect()['usage'])

    def test_malformed_tool_terminal_never_establishes_retry_coverage(self):
        self.opened()
        self.usage('first', 30, 20)
        self.event('session.content.appended.1', self.tool())
        valid = dict(attempt='retry', call='outer', outcome=dict(kind='completed'))
        mutations = [
            (('outcome',), None), (('outcome',), {}),
            (('outcome', 'kind'), 'unlisted'), (('unexpected',), True),
            (('call',), []), (('attempt',), ''),
            (('outcome',), dict(kind='failed')),
            (('outcome',), dict(kind='failed', reason='unlisted')),
            (('outcome',), dict(kind='cancelled', by='unlisted')),
            (('outcome',), dict(kind='result-missing', output={})),
            (('outcome', 'output'), None),
            (('outcome', 'output'), dict(id='art', length=2, digest='digest')),
            (('outcome', 'output'), dict(id='art', length=True, digest='digest',
                                        disposition='complete')),
            (('outcome', 'output'), dict(id='art', length=2, digest='',
                                        disposition='complete')),
            (('outcome', 'output'), dict(id='art', length=2, digest='digest',
                                        disposition='truncated', captured=1)),
            (('outcome', 'output'), dict(id='art', length=2, digest='digest',
                                        disposition='complete', captured=2)),
            (('outcome', 'output'), dict(id='art', length=2, digest='digest',
                                        disposition='complete', unknown=True)),
        ]
        missing = copy.deepcopy(valid)
        missing.pop('outcome')
        bodies = [missing] + [mutated(valid, path, value) for path, value in mutations]
        for body in bodies:
            with self.subTest(body=body):
                self.connection.execute('DELETE FROM event WHERE seq>=3')
                self.seq = 3
                self.usage('retry', 70, 50)
                self.event('session.tool.terminal.1', body)
                self.assertIsNone(self.collect()['usage'])

    def test_malformed_appended_item_never_establishes_a_top_level_call(self):
        self.opened()
        self.usage('first', 30, 20)
        valid = self.tool()
        mutations = [
            (('unexpected',), True), (('message',), 'foreign'), (('item',), None),
            (('item', 'ordinal'), True), (('item', 'ordinal'), -1),
            (('item', 'ordinal'), 2**64), (('item', 'kind'), 'unlisted'),
            (('item', 'call'), ''), (('item', 'call'), []),
            (('item', 'payload'), None), (('item', 'payload', 'id'), ''),
            (('item', 'payload', 'digest'), None),
            (('item', 'payload', 'length'), True),
            (('item', 'payload', 'length'), 2**64),
            (('item', 'payload', 'unknown'), 'value'),
            (('item', 'disposition'), 'unlisted'),
            (('item', 'captured'), 2),  # A complete item cannot also be truncated.
            (('item', 'unknown'), 'value'),
        ]
        bodies = [mutated(valid, path, value) for path, value in mutations]
        for field in ('ordinal', 'call', 'payload', 'disposition'):
            missing = copy.deepcopy(valid)
            missing['item'].pop(field)
            bodies.append(missing)
        for field in ('id', 'length', 'digest'):
            missing = copy.deepcopy(valid)
            missing['item']['payload'].pop(field)
            bodies.append(missing)
        for body in bodies:
            with self.subTest(body=body):
                self.connection.execute('DELETE FROM event WHERE seq>=2')
                self.seq = 2
                self.event('session.content.appended.1', body)
                self.usage('retry', 70, 50)
                self.event('session.tool.terminal.1',
                           dict(attempt='retry', call='outer', outcome=dict(kind='completed')))
                self.assertIsNone(self.collect()['usage'])

    def test_all_native_terminal_outcomes_can_bind_a_retry(self):
        self.opened()
        self.usage('first', 30, 20)
        self.event('session.content.appended.1', self.tool())
        complete = dict(id='art_output', length=2, digest='digest_output', disposition='complete')
        truncated = dict(complete, disposition='truncated', captured=2)
        outcomes = [dict(kind='completed'), dict(kind='completed', output=complete),
                    dict(kind='completed', output=truncated), dict(kind='result-missing')]
        outcomes += [dict(kind='failed', reason=reason, detail=complete)
                     for reason in ['authority-unavailable', 'effect-failed',
                                    'capture-failed', 'effect-unknown']]
        outcomes += [dict(kind='cancelled', by=by) for by in ['user', 'managed-shutdown']]
        for outcome in outcomes:
            with self.subTest(outcome=outcome):
                self.connection.execute('DELETE FROM event WHERE seq>=3')
                self.seq = 3
                self.usage('retry', 70, 50)
                self.event('session.tool.terminal.1',
                           dict(attempt='retry', call='outer', outcome=outcome))
                self.assertEqual(self.collect()['usage']['input_tokens'], 100)

    def test_native_truncated_appended_item_keeps_its_binding(self):
        self.opened()
        self.usage('first', 30, 20)
        tool = self.tool()
        tool['item'].update(disposition='truncated', captured=2)
        self.event('session.content.appended.1', tool)
        self.usage('retry', 70, 50)
        self.event('session.tool.terminal.1',
                   dict(attempt='retry', call='outer', outcome=dict(kind='completed')))
        self.assertEqual(self.collect()['usage']['input_tokens'], 100)

    def test_invalid_block_fields_never_establish_a_retry(self):
        self.opened()
        self.usage('first', 30, 20)
        valid = dict(attempt='retry', message='msg_first', block=0, kind='reasoning')
        mutations = [(('block',), True), (('block',), -1), (('block',), 2**32),
                     (('message',), 'foreign'), (('kind',), 'tool'), (('unexpected',), True)]
        for path, value in mutations:
            with self.subTest(path=path, value=value):
                self.connection.execute('DELETE FROM event WHERE seq>=2')
                self.seq = 2
                self.event('session.content-block.opened.1', mutated(valid, path, value))
                self.usage('retry', 70, 50)
                self.assertIsNone(self.collect()['usage'])

    def test_native_v2_opening_and_invalid_selection_bindings(self):
        valid = dict(attempt='first', step=1, message='msg_first',
                     selection=dict(agent='agent', provider='openai', model='selected', variant='low'))
        self.event('session.assistant-message.opened.2', valid)
        self.usage('first', 30, 20)
        self.assertEqual(self.collect()['usage']['input_tokens'], 30)
        mutations = [(('step',), 0), (('step',), 2**32), (('unexpected',), True),
                     (('selection',), None), (('selection', 'model'), ''),
                     (('selection', 'variant'), None), (('selection', 'unexpected'), True)]
        for path, value in mutations:
            with self.subTest(path=path, value=value):
                self.connection.execute('DELETE FROM event')
                self.seq = 0
                self.event('session.assistant-message.opened.2', mutated(valid, path, value))
                self.usage('first', 30, 20)
                self.assertIsNone(self.collect()['usage'])


def mutated(original, path, value):
    result = copy.deepcopy(original)
    target = result
    for key in path[:-1]:
        target = target[key]
    target[path[-1]] = value
    return result


if __name__ == '__main__':
    unittest.main()
