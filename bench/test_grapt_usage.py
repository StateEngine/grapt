"""No-model witnesses for durable benchmark accounting and its cleanup boundary."""
import hashlib
import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import unittest
from unittest.mock import patch

import run
import grapt_usage


def usage(total=33, output=4, read=20, write=3, phase='terminal'):
    return dict(phase=phase, **{key: {'count': value, 'provenance': 'provider-reported'}
                for key, value in {'inputTotalTokens': total, 'inputFreshTokens': total-read-write,
                                   'outputTokens': output, 'cacheReadInputTokens': read,
                                   'cacheWriteInputTokens': write}.items()})


class UsageTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.database = self.root/'database.sqlite3'
        self.connection = sqlite3.connect(self.database)
        self.addCleanup(self.connection.close)
        # These disposable fixtures test accounting reads, not crash durability.
        self.connection.execute('PRAGMA synchronous=OFF')
        self.connection.execute('CREATE TABLE event(aggregate_family, aggregate_id, seq, type, data)')
        self.seq = 0

    def event(self, kind, body, session='ses_target'):
        self.connection.execute('INSERT INTO event VALUES (?, ?, ?, ?, ?)',
                                ('session', session, self.seq, kind, json.dumps(body)))
        self.seq += 1
        self.connection.commit()

    def opened(self, attempt='one', session='ses_target'):
        self.event('session.assistant-message.opened.1',
                   {'attempt': attempt, 'step': 1, 'message': 'msg_'+attempt}, session)

    def observed(self, value=None, attempt='one', session='ses_target'):
        self.event('session.usage.observed.2', {'attempt': attempt, 'usage': value or usage()}, session)

    def collect(self):
        return grapt_usage.collect(self.database, 'ses_target')

    def test_session_filter_phase_reconciliation_and_partition(self):
        self.opened()
        self.observed(usage(total=30, output=1, phase='during-stream'))
        self.observed()
        self.observed()  # Duplicate observation must not charge twice.
        self.observed(usage(total=30, output=1, phase='during-stream'))
        self.opened('two')
        self.observed(usage(total=10, output=2, read=0, write=0), 'two')
        self.opened('foreign', 'ses_foreign')
        self.observed(usage(total=100), 'foreign', 'ses_foreign')
        result = self.collect()
        self.assertEqual(result['usage'], {'input_tokens': 43, 'cached_input_tokens': 20, 'output_tokens': 6})
        self.assertEqual(result['cache_write_tokens'], 3)
        self.assertEqual(result['cache_write_tokens_observed'], 3)
        self.assertEqual(result['grapt_usage_evidence']['attempts'], 2)

    def test_first_call_uses_opening_order_not_delayed_usage_arrival(self):
        self.opened('first')
        self.event('session.compaction.call.started.1', {'attempt': 'summary'})
        self.event('session.compaction.call.usage.1', {'attempt': 'summary', 'usage': usage(total=40, read=25)})
        self.observed(attempt='first')
        result = self.collect()
        self.assertEqual([c['identity'] for c in result['call_usage']], ['first', 'summary'])
        self.assertFalse(result['call_usage'][0]['compaction_summary'])
        self.assertTrue(result['call_usage'][1]['compaction_summary'])

    def test_history_fork_does_not_charge_copied_parent_usage_to_the_child(self):
        self.opened('parent', 'ses_parent')
        self.observed(attempt='parent', session='ses_parent')
        parent_seq = self.seq - 1
        self.event('session.forked.2', {'sessionID': 'ses_target',
                   'parentID': 'ses_parent', 'parentSeq': parent_seq})
        # The lifecycle owner remints message IDs, retaining Physical Attempts
        # and their usage so the copied conversation still renders correctly.
        self.event('session.assistant-message.opened.1',
                   {'attempt': 'parent', 'step': 1, 'message': 'msg_copied'})
        self.observed(attempt='parent')
        self.opened('child')
        self.observed(usage(total=10, output=2, read=0, write=0), 'child')
        result = self.collect()
        self.assertEqual(result['usage'],
                         {'input_tokens': 10, 'cached_input_tokens': 0, 'output_tokens': 2})
        self.assertEqual([call['identity'] for call in result['call_usage']], ['child'])
        self.assertEqual(result['cache_write_tokens'], 0)

    def test_copied_parent_usage_cannot_fill_absent_child_usage(self):
        self.opened('parent', 'ses_parent')
        self.observed(attempt='parent', session='ses_parent')
        self.event('session.forked.2', {'sessionID': 'ses_target',
                   'parentID': 'ses_parent', 'parentSeq': self.seq - 1})
        self.event('session.assistant-message.opened.1',
                   {'attempt': 'parent', 'step': 1, 'message': 'msg_copied'})
        self.observed(attempt='parent')
        self.opened('child')
        calls = grapt_usage.request_usage(self.database, 'ses_target')
        self.assertEqual([call['attempt'] for call in calls], ['child'])
        self.assertEqual(calls[0]['usage_status'], 'unavailable')
        self.assertIsNone(self.collect()['usage'])

    def test_fork_usage_must_match_the_frozen_parent(self):
        self.opened('parent', 'ses_parent')
        self.observed(attempt='parent', session='ses_parent')
        self.event('session.forked.2', {'sessionID': 'ses_target',
                   'parentID': 'ses_parent', 'parentSeq': self.seq - 1})
        self.event('session.assistant-message.opened.1',
                   {'attempt': 'parent', 'step': 1, 'message': 'msg_copied'})
        self.observed(usage(output=9, phase='reconciled'), attempt='parent', session='ses_parent')
        self.observed(usage(output=9, phase='reconciled'), attempt='parent')
        self.opened('child'); self.observed(attempt='child')
        result = self.collect()
        self.assertIsNone(result['usage'])
        self.assertIn('frozen parent', result['grapt_usage_evidence']['reason'])

    def test_copied_tool_bindings_do_not_become_child_retries(self):
        self.opened('parent', 'ses_parent'); self.observed(attempt='parent', session='ses_parent')
        self.event('session.forked.2', {'sessionID': 'ses_target',
                   'parentID': 'ses_parent', 'parentSeq': self.seq - 1})
        self.event('session.assistant-message.opened.1',
                   {'attempt': 'parent', 'step': 1, 'message': 'msg_copied'})
        self.event('session.content.appended.1', {'message': 'msg_copied', 'item': {
            'ordinal': 0, 'kind': 'tool', 'call': 'call_copied', 'disposition': 'complete',
            'payload': {'id': 'artifact_copied', 'length': 1, 'digest': 'fixture'}}})
        self.observed(attempt='parent')
        self.opened('child'); self.observed(attempt='child')
        self.assertEqual(self.collect()['usage']['input_tokens'], 33)

    def test_fork_of_a_fork_excludes_both_generations_of_copied_calls(self):
        self.opened('root', 'ses_root'); self.observed(attempt='root', session='ses_root')
        self.event('session.forked.2', {'sessionID': 'ses_parent',
                   'parentID': 'ses_root', 'parentSeq': self.seq - 1}, 'ses_parent')
        self.event('session.assistant-message.opened.1',
                   {'attempt': 'root', 'step': 1, 'message': 'msg_root_copy'}, 'ses_parent')
        self.observed(attempt='root', session='ses_parent')
        self.opened('parent', 'ses_parent'); self.observed(attempt='parent', session='ses_parent')
        self.event('session.forked.2', {'sessionID': 'ses_target',
                   'parentID': 'ses_parent', 'parentSeq': self.seq - 1})
        for attempt in ['root', 'parent']:
            self.event('session.assistant-message.opened.1',
                       {'attempt': attempt, 'step': 1, 'message': 'msg_copy_'+attempt})
            self.observed(attempt=attempt)
        self.opened('child'); self.observed(attempt='child')
        self.assertEqual(self.collect()['usage']['input_tokens'], 33)

    def test_unavailable_or_malformed_fork_ancestry_never_qualifies_usage(self):
        for mutation in ['parent-absent', 'self-parent', 'boolean-boundary', 'unknown-version', 'repeated', 'invalid-from']:
            with self.subTest(mutation=mutation):
                self.connection.execute('DELETE FROM event')
                self.opened('parent', 'ses_parent'); self.observed(attempt='parent', session='ses_parent')
                fork = {'sessionID': 'ses_target', 'parentID': 'ses_parent', 'parentSeq': self.seq - 1}
                if mutation == 'parent-absent': fork['parentID'] = 'ses_missing'
                elif mutation == 'self-parent': fork['parentID'] = 'ses_target'
                elif mutation == 'boolean-boundary': fork['parentSeq'] = True
                elif mutation == 'invalid-from': fork['from'] = 'not-a-message'
                kind = 'session.forked.3' if mutation == 'unknown-version' else 'session.forked.2'
                self.event(kind, fork)
                if mutation == 'repeated': self.event(kind, fork)
                self.opened('child'); self.observed(attempt='child')
                self.assertIsNone(self.collect()['usage'])

    def test_fork_optional_message_boundary_retains_its_frozen_sequence(self):
        self.opened('parent', 'ses_parent'); self.observed(attempt='parent', session='ses_parent')
        self.event('session.forked.2', {'sessionID': 'ses_target', 'from': 'msg_parent',
                   'parentID': 'ses_parent', 'parentSeq': self.seq - 1})
        self.event('session.assistant-message.opened.1',
                   {'attempt': 'parent', 'step': 1, 'message': 'msg_copied'})
        self.observed(attempt='parent')
        self.opened('child'); self.observed(attempt='child')
        self.assertEqual(self.collect()['usage']['input_tokens'], 33)

    def test_missing_fork_boundary_is_unavailable_in_both_accounting_views(self):
        for boundary in [999999, 2]:
            with self.subTest(boundary=boundary):
                self.connection.execute('DELETE FROM event'); self.seq = 0
                self.opened('parent', 'ses_parent'); self.observed(attempt='parent', session='ses_parent')
                # A gap below the tail is not a valid boundary either.
                self.seq = 4
                self.opened('later', 'ses_parent'); self.observed(attempt='later', session='ses_parent')
                self.event('session.forked.2', {'sessionID': 'ses_target',
                           'parentID': 'ses_parent', 'parentSeq': boundary})
                self.event('session.assistant-message.opened.1',
                           {'attempt': 'parent', 'step': 1, 'message': 'msg_copied'})
                self.observed(attempt='parent')
                self.opened('child'); self.observed(attempt='child')
                self.assertIsNone(self.collect()['usage'])
                with self.assertRaisesRegex(ValueError, 'boundary'):
                    grapt_usage.request_usage(self.database, 'ses_target')

    def test_empty_fork_prefix_does_not_subtract_any_parent_calls(self):
        self.opened('parent', 'ses_parent'); self.observed(attempt='parent', session='ses_parent')
        self.event('session.forked.2', {'sessionID': 'ses_target',
                   'parentID': 'ses_parent', 'parentSeq': -1})
        self.opened('child'); self.observed(attempt='child')
        self.assertEqual(self.collect()['usage']['input_tokens'], 33)
        calls = grapt_usage.request_usage(self.database, 'ses_target')
        self.assertEqual([call['attempt'] for call in calls], ['child'])

    def test_fresh_partition_can_supply_missing_inclusive_total(self):
        self.opened(); value = usage(); value.pop('inputTotalTokens'); self.observed(value)
        self.assertEqual(self.collect()['usage']['input_tokens'], 33)

    def test_C04_compactor_provider_call_counts_toward_the_whole_row(self):
        self.opened(); self.observed()
        self.event('session.compaction.call.started.1', {'attempt': 'summary', 'compaction': 'compact', 'step': 2})
        self.event('session.compaction.call.usage.1', {'attempt': 'summary', 'usage': usage(total=40, read=25)})
        result = self.collect()
        self.assertEqual(result['usage']['input_tokens'], 73)
        self.assertEqual(result['usage']['cached_input_tokens'], 45)
        self.assertEqual(result['grapt_usage_evidence']['compaction_attempts'], 1)
        self.assertAlmostEqual(result['cache_read_share'], 45/73)

    def test_absent_is_unknown_but_observed_zero_is_known(self):
        self.opened(); self.observed(usage(total=0, output=0, read=0, write=0))
        self.assertEqual(self.collect()['usage']['input_tokens'], 0)
        self.assertIsNone(self.collect()['cache_read_share'])
        self.opened('no_usage')
        self.assertIsNone(self.collect()['usage'])

    def test_missing_leaf_and_estimated_or_nonterminal_are_unknown(self):
        self.opened()
        for mutation in ['absent', 'estimated', 'stream']:
            with self.subTest(mutation=mutation):
                self.connection.execute("DELETE FROM event WHERE type LIKE 'session.usage%'")
                value = usage()
                if mutation == 'absent': value.pop('cacheWriteInputTokens')
                elif mutation == 'estimated': value['outputTokens']['provenance'] = 'harness-estimated'
                else: value['phase'] = 'during-stream'
                self.observed(value)
                self.assertIsNone(self.collect()['usage'])

    def test_conflicting_same_phase_is_corruption(self):
        self.opened(); self.observed(); self.observed(usage(output=5))
        result = self.collect()
        self.assertIsNone(result['usage'])
        self.assertIn('Conflicting', result['grapt_usage_evidence']['reason'])

    def test_later_phase_replaces_each_leaf(self):
        self.opened(); self.observed(); self.observed(usage(output=7, phase='reconciled'))
        self.assertEqual(self.collect()['usage']['output_tokens'], 7)

    def test_boolean_negative_partition_and_legacy_refuse(self):
        self.opened()
        for count in [True, -1, 2**63]:
            self.connection.execute("DELETE FROM event WHERE type LIKE 'session.usage%'")
            value = usage(); value['outputTokens']['count'] = count; self.observed(value)
            self.assertIsNone(self.collect()['usage'])
        self.connection.execute("DELETE FROM event WHERE type LIKE 'session.usage%'")
        value = usage(); value['inputFreshTokens']['count'] = 11; self.observed(value)
        self.assertIsNone(self.collect()['usage'])
        self.connection.execute("DELETE FROM event WHERE type LIKE 'session.usage%'")
        self.event('session.usage.observed.1', {'attempt': 'one', 'usage': usage()})
        self.assertIn('legacy', self.collect()['grapt_usage_evidence']['reason'])

    def test_absent_unreadable_and_foreign_session_do_not_become_zero(self):
        self.assertIsNone(grapt_usage.collect(self.root/'missing', 'ses_target')['usage'])
        corrupt = self.root/'corrupt'; corrupt.write_bytes(b'not sqlite')
        self.assertIsNone(grapt_usage.collect(corrupt, 'ses_target')['usage'])
        self.assertIsNone(grapt_usage.collect(self.database, 'ses_missing')['usage'])
        self.assertIsNone(grapt_usage.collect(self.database, None)['usage'])

    def test_symlink_and_oversized_payload_or_row_census_refuse(self):
        self.opened(); self.observed()
        link = self.root/'link'; link.symlink_to(self.database)
        self.assertIsNone(grapt_usage.collect(link, 'ses_target')['usage'])
        with patch.object(grapt_usage, 'MAX_ROW_BYTES', 10):
            self.assertIsNone(self.collect()['usage'])
        with patch.object(grapt_usage, 'MAX_ROWS', 1):
            self.assertIsNone(self.collect()['usage'])
        with patch.object(grapt_usage, 'MAX_BYTES', 10):
            self.assertIsNone(self.collect()['usage'])

    def test_wal_snapshot_is_read_only_and_retains_auditable_observations(self):
        self.connection.execute('PRAGMA journal_mode=WAL')
        self.opened(); self.observed()
        before = self.database.read_bytes()
        path = self.root/'usage.json'
        result = grapt_usage.collect(self.database, 'ses_target', path)
        self.assertEqual(self.database.read_bytes(), before)
        self.assertEqual(result['grapt_usage_evidence']['sha256'], hashlib.sha256(path.read_bytes()).hexdigest())
        saved = json.loads(path.read_text())
        self.assertEqual(saved['observations'][0]['fields']['inputTotalTokens']['count'], 33)
        self.assertNotIn('observations', result)

    def test_regular_and_C04_session_identity_reaches_store_reader(self):
        for event in [{'type': 'benchmark.grapt.completed', 'session': 'ses_target'},
                      {'type': 'benchmark.automatic.completed', 'session': 'ses_target'}]:
            path = self.root/'native.jsonl'; path.write_text(json.dumps(event)+'\n')
            self.assertEqual(run.telemetry(path, 'grapt')['grapt_session'], 'ses_target')

    def test_failed_C04_keeps_owned_identity_and_multiple_sessions_refuse_attribution(self):
        path = self.root/'native.jsonl'
        opening = {'direction': 'server', 'message': {'result': {'sessionId': 'ses_target'}}}
        failed = {'type': 'benchmark.automatic.completed', 'session': None}
        path.write_text(json.dumps(opening)+'\n'+json.dumps(failed)+'\n')
        self.assertEqual(run.telemetry(path, 'grapt')['grapt_session'], 'ses_target')
        with path.open('a') as output:
            output.write(json.dumps({'result': {'sessionId': 'ses_foreign'}})+'\n')
        self.assertIsNone(run.telemetry(path, 'grapt')['grapt_session'])

    def test_usage_operation_identity_and_exact_token_bound(self):
        self.observed()
        self.assertIsNone(self.collect()['usage'])
        self.connection.execute('DELETE FROM event')
        self.opened(); self.observed(usage(total=grapt_usage.MAX_TOKENS, read=0, write=0))
        self.assertEqual(self.collect()['usage']['input_tokens'], grapt_usage.MAX_TOKENS)
        self.opened('two'); self.observed(usage(read=0, write=0), 'two')
        self.assertIsNone(self.collect()['usage'])

    def test_row_collects_before_private_store_cleanup(self):
        code = """import json,sqlite3
from pathlib import Path
Path('answer.py').write_text('value = 1\\n')
p=Path('{service_root}');p.mkdir()
c=sqlite3.connect(p/'database.sqlite3')
c.execute('PRAGMA synchronous=OFF')
c.execute('CREATE TABLE event(aggregate_family,aggregate_id,seq,type,data)')
for seq,(kind,data) in enumerate([('session.assistant-message.opened.1',{'attempt':'one','step':1,'message':'msg_one'}),('session.usage.observed.2',{'attempt':'one','usage':USAGE})]):
 c.execute('INSERT INTO event VALUES (?,?,?,?,?)',('session','ses_target',seq,kind,json.dumps(data)))
c.commit();c.close()
print('GRAPT_PROVIDER_DISPATCH attempt=one at_ms=10 cache_request='+json.dumps({
 'session':'ses_target','metadata':{'cacheRequestChange':'first_observation',
 'cacheRequestChangedItem':None,'cacheRequestPreviousAttempt':None,'cacheRequestBodyBytes':1000,
 'cacheRequestToolsBytes':300,'cacheRequestInstructionsBytes':200}}))
print(json.dumps({'type':'benchmark.grapt.completed','session':'ses_target'}))
""".replace('USAGE', repr(usage()))
        adapter = {'model': 'none', 'auth': 'none', 'harness': 'grapt',
                   'argv': [sys.executable, '-c', code]}
        task = {'id': 'probe', 'category': 'bug', 'held_out': False,
                'files': {'answer.py': 'value = 0\n'}, 'prompt': 'Set value', 'hidden_tests': ''}
        with patch.object(run, 'agent_sandbox', side_effect=lambda argv, *a, **kw: argv), \
                patch.object(run, 'hidden_checks', return_value=('completed', 0)):
            result = run.attempt(task, 'grapt', adapter, self.root/'results', 60, {})
        self.assertTrue(result['success'],
                        f"status={result.get('status')} error={result.get('error')} exit={result.get('exit_code')}")
        self.assertEqual(result['usage']['input_tokens'], 33)
        self.assertTrue((self.root/'results/grapt/probe/usage.json').is_file())
        self.assertEqual(result['request_cache']['status'], 'complete')
        self.assertEqual(result['request_cache']['calls'][0]['attempt'], 'one')
        self.assertEqual(result['request_cache']['calls'][0]['usage']['inputTotalTokens']['count'], 33)
        self.assertEqual(result['request_cache']['calls'][0]['request']['change'], 'first_observation')
        self.assertEqual(result['request_cache']['provider_miss_cause'], 'unreported')
        self.assertTrue((self.root/'results/grapt/probe/request-cache.json').is_file())


if __name__ == '__main__':
    unittest.main()
