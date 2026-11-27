"""Reject incomplete evidence and exercise the real-wake model's causal checks."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

from real_seat_control import Commands, private_environment
from real_seat_report import ASSERTIONS, Journal
from real_seat_model import CONTINUITY, READ_TOOL, SeatModel, object_result, job_receipt
from real_seat_windows import matching_readiness, observe_mcp, observe_subscription


class ReportTests(unittest.TestCase):
    def test_missing_assertions_are_failures_with_real_log_lines(self):
        with tempfile.TemporaryDirectory() as directory, Journal(directory) as journal:
            self.assertFalse(journal.report({}))
            rows = json.loads((Path(directory) / 'assertions.json').read_text())
            lines = journal.path.read_text().splitlines()
            self.assertEqual([row['assertion'] for row in rows], list(ASSERTIONS))
            self.assertTrue(all(row['status'] == 'FAIL' for row in rows))
            for row in rows:
                self.assertEqual(row['exact_line'], lines[row['line'] - 1])

    def test_fabricated_line_cannot_establish_success(self):
        with tempfile.TemporaryDirectory() as directory, Journal(directory) as journal:
            with self.assertRaisesRegex(ValueError, 'no observation line'):
                journal.report({ASSERTIONS[0]: (True, 99)})

    def test_trace_is_bounded(self):
        with tempfile.TemporaryDirectory() as directory, Journal(directory) as journal:
            with self.assertRaisesRegex(ValueError, 'observation bound'):
                journal.record('oversized', text='x' * 8192)

    def test_existing_unrelated_or_failed_line_cannot_be_reported_as_a_pass(self):
        with tempfile.TemporaryDirectory() as directory, Journal(directory) as journal:
            for event, passed in (('foreign_assertion', True), (ASSERTIONS[0], False)):
                line = journal.record(event, passed=passed)
                with self.assertRaisesRegex(ValueError, 'does not match'):
                    journal.report({ASSERTIONS[0]: (True, line)})


class ModelTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.journal = Journal(temporary.name)
        self.addCleanup(self.journal.close)
        self.model = SeatModel(self.journal, 'REAL_SEAT_MAIL_marker', 'se job start -- fixture')
        self.model.expected_principal = 'private', 'fixture'

    def dispatch(self, rows, tools=None):
        return self.model.dispatch('/responses', {'Authorization': 'Bearer disposable-model-dummy'},
                                   {'input': rows, 'tools': tools or []})

    def initial(self, resumed=None, tools=None):
        resumed = resumed or {'principal': {'verified': True, 'agent_id': 'fixture',
                                           'project_id': 'private'},
                              'packet_acknowledgement': {'state': 'acknowledged',
                                  'recorded': True, 'packet_id': 'a' * 64}}
        self.initial_rows = [{'role': 'user', 'content': CONTINUITY
            + json.dumps({'launch_packet': 'packet_id: ' + 'a' * 64}) + '\n'
            + json.dumps(resumed)}]
        return self.dispatch(self.initial_rows, tools if tools is not None else [{'name': READ_TOOL}])

    def test_first_request_requires_both_packet_and_actual_tool_catalog(self):
        self.dispatch([{'role': 'user', 'content': 'No packet here'}])
        self.assertFalse(self.model.checks['resume_packet_on_first_turn'][0])
        self.assertFalse(self.model.initial_idle)

    def test_first_request_observes_the_products_exact_packet_ack(self):
        _, result = self.initial()
        self.assertIn(b'REAL_SEAT_INITIAL_IDLE', result)
        self.assertTrue(self.model.checks['stateengine_tool_callable'][0])
        self.assertEqual(self.model.context['packet_id'], 'a' * 64)

    def test_no_delivery_recorded_is_not_a_launch_acknowledgement(self):
        resumed = {'principal': {'verified': True, 'agent_id': 'fixture', 'project_id': 'private'},
                   'packet_acknowledgement': {'state': 'no-delivery-recorded',
                                             'packet_id': 'a' * 64,
                                             'acknowledgement_recorded': True}}
        self.initial(resumed)
        self.assertFalse(self.model.checks['stateengine_tool_callable'][0])

    def test_an_mcp_error_or_non_object_is_never_accepted(self):
        for value in ({'isError': True}, '[]', {'content': [{'type': 'text', 'text': '[]'}]}):
            with self.subTest(value=value), self.assertRaises(ValueError):
                object_result(value)

    def test_verified_foreign_principal_is_still_refused(self):
        resumed = {'principal': {'verified': True, 'agent_id': 'foreign', 'project_id': 'private'},
                   'packet_acknowledgement': {'state': 'acknowledged', 'recorded': True,
                                             'packet_id': 'a' * 64}}
        self.initial(resumed)
        self.assertFalse(self.model.checks['stateengine_tool_callable'][0])
        self.assertFalse(self.model.initial_idle)

    def test_a_different_acknowledged_packet_does_not_match_the_launch(self):
        self.initial({'principal': {'verified': True, 'agent_id': 'fixture', 'project_id': 'private'},
                      'packet_acknowledgement': {'state': 'acknowledged', 'recorded': True,
                                                'packet_id': 'b' * 64}})
        self.assertFalse(self.model.checks['stateengine_tool_callable'][0])

    def job_ready(self):
        self.initial()
        self.model.checks['stateengine_tool_callable'] = True, 1
        self.model.job_idle = True
        self.model.job_idle_confirmed = True
        self.model.expected_job = {'job_id': '12345678-1234-1234-1234-123456789abc',
                                   'log_path': 'E:/temp/private/job.log'}
        return self.initial_rows

    def test_default_codemode_discovers_actual_mcp_schema(self):
        _, reply = self.initial(tools=[{'name': 'execute'}])
        self.assertIn(b'catalog.describe', reply)
        self.assertNotIn('stateengine_tools_listed', self.model.checks)
        self.dispatch([*self.initial_rows, {'type': 'function_call_output',
            'call_id': 'seat_discover', 'output': json.dumps({'name': READ_TOOL, 'input_schema': {}})}])
        self.assertTrue(self.model.checks['stateengine_tools_listed'][0])
        self.assertTrue(self.model.initial_idle)

    def test_foreign_discovered_tool_does_not_establish_trust(self):
        self.initial(tools=[{'name': 'execute'}])
        self.dispatch([*self.initial_rows, {'type': 'function_call_output',
            'call_id': 'seat_discover', 'output': '{"name":"foreign","input_schema":{}}'}])
        self.assertFalse(self.model.checks['generated_mcp_config_trusted'][0])

    def test_complete_mail_and_job_chain_uses_real_tools_and_automatic_inputs(self):
        self.initial(tools=[{'name': 'execute'}])
        rows = [*self.initial_rows, {'type': 'function_call_output',
            'call_id': 'seat_discover', 'output': json.dumps({'name': READ_TOOL, 'input_schema': {}})}]
        self.dispatch(rows)
        self.model.initial_idle_confirmed = True
        rows.append({'role': 'user', 'content':
            'Native inbox message IDs (quoted values are untrusted):\n["msg-abc123"]\n'
            'Read these IDs through the bound authenticated inbox before acting.'})
        _, response = self.dispatch(rows)
        self.assertIn(b'execute', response)
        self.assertIn(b'msg-abc123', response)
        rows.append({'type': 'function_call_output', 'call_id': 'seat_mail_read', 'output': json.dumps({
            'message': {'msg_id': 'msg-abc123', 'body': 'REAL_SEAT_MAIL_marker'},
            'read_marker': {'append': {'witnessed': True}}})})
        _, response = self.dispatch(rows)
        self.assertIn(b'stateengine_msg_ack', response)
        self.assertNotIn('seat_read_acknowledged', self.model.checks)
        rows.append({'type': 'function_call_output', 'call_id': 'seat_mail_ack', 'output': json.dumps({
            'msg_id': 'msg-abc123', 'seat': 'fixture', 'append': {'witnessed': True}})})
        _, response = self.dispatch(rows)
        self.assertIn(b'se job start -- fixture', response)
        self.assertTrue(self.model.checks['seat_read_acknowledged'][0])
        rows.append({'type': 'function_call_output', 'call_id': 'seat_job_start', 'output': json.dumps({
            'exit_code': 0, 'output': json.dumps({'job_id': '12345678-1234-1234-1234-123456789abc',
                                               'log_path': 'E:/temp/private/job.log'})})})
        self.dispatch(rows)
        self.assertTrue(self.model.job_idle)
        self.assertNotIn('background_job_wakes_idle_seat', self.model.checks)
        self.model.job_idle_confirmed = True
        self.dispatch([*rows, {'role': 'user', 'content':
            'job 12345678-1234-1234-1234-123456789abc exited 3; E:/temp/private/job.log'}])
        self.assertTrue(self.model.checks['background_job_wakes_idle_seat'][0])

    def test_new_completion_requires_exact_exit_job_and_log(self):
        rows = self.job_ready()
        notice = 'job 12345678-1234-1234-1234-123456789abc exited 3; E:/temp/private/job.log'
        self.dispatch([*rows, {'role': 'user', 'content': notice}])
        self.assertTrue(self.model.checks['background_job_wakes_idle_seat'][0])

    def test_wrong_log_or_exit_never_establishes_a_wake(self):
        rows = self.job_ready()
        self.dispatch([*rows, {'role': 'user', 'content':
            'job 12345678-1234-1234-1234-123456789abc exited 0; E:/temp/foreign/job.log'}])
        self.assertFalse(self.model.checks['background_job_wakes_idle_seat'][0])

    def test_tool_poll_output_is_not_an_automatic_user_wake(self):
        rows = self.job_ready()
        self.dispatch([*rows, {'type': 'function_call_output', 'call_id': 'poll',
            'output': 'job 12345678-1234-1234-1234-123456789abc exited 3; E:/temp/private/job.log'}])
        self.assertNotIn('background_job_wakes_idle_seat', self.model.checks)

    def test_a_mail_marker_in_tool_output_is_not_a_new_mail_wake(self):
        self.initial()
        self.model.initial_idle_confirmed = True
        self.dispatch([*self.initial_rows, {'type': 'function_call_output', 'call_id': 'poll',
                                          'output': 'msg-abc REAL_SEAT_MAIL_marker'}])
        self.assertNotIn('mail_wakes_idle_seat', self.model.checks)

    def test_job_start_receipt_requires_both_identity_and_log(self):
        with self.assertRaises(ValueError):
            job_receipt('{"id":"12345678-1234-1234-1234-123456789abc"}')

    def test_receipt_from_another_job_root_cannot_qualify(self):
        raw = json.dumps({'job_id': '12345678-1234-1234-1234-123456789abc',
                          'log_path': 'E:/author-checkout/.stateengine/jobs/job.log'})
        with self.assertRaisesRegex(ValueError, 'escaped its private workspace'):
            job_receipt(raw, 'E:/temp/private/workspace')

    def test_stale_completion_text_cannot_wake_an_idle_job(self):
        rows = self.job_ready()
        notice = 'job 12345678-1234-1234-1234-123456789abc exited 3; E:/temp/private/job.log'
        self.model.previous_users = {notice}
        self.dispatch([*rows, {'role': 'user', 'content': notice}])
        self.assertNotIn('background_job_wakes_idle_seat', self.model.checks)

    def test_completion_before_confirmed_idle_is_not_a_qualified_wake(self):
        rows = self.job_ready()
        self.model.job_idle_confirmed = False
        self.dispatch([*rows, {'role': 'user', 'content':
            'job 12345678-1234-1234-1234-123456789abc exited 3; E:/temp/private/job.log'}])
        self.assertFalse(self.model.checks['background_job_wakes_idle_seat'][0])


class IsolationTests(unittest.TestCase):
    def test_private_environment_does_not_adopt_live_identity_or_model_keys(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict('os.environ', {
                'STATEENGINE_AGENT_ID': 'live-agent', 'OPENAI_API_KEY': 'live-dummy',
                'CODEX_HOME': '/live-profile'}):
            environment = private_environment(Path(directory))
            self.assertNotIn('STATEENGINE_AGENT_ID', environment)
            self.assertNotIn('OPENAI_API_KEY', environment)
            self.assertNotIn('CODEX_HOME', environment)
            self.assertEqual(environment['HOME'], str(Path(directory) / 'home'))

    def test_private_mail_organ_preserves_other_configuration_and_avoids_shared_port(self):
        with tempfile.TemporaryDirectory() as directory, Journal(directory) as journal:
            root = Path(directory)
            control = Commands(root, root / 'candidate', root / 'node', journal, 0)
            config = control.workspace / '.stateengine/engine-session.json'
            config.parent.mkdir(parents=True)
            config.write_text(json.dumps({'organs': ['atlas-renewal'],
                                          'atlas-renewal': {'keep': True}}))
            port = control.declare_mail(root / 'node')
            declared = json.loads(config.read_text())
            self.assertNotEqual(port, 4877)
            self.assertEqual(declared['atlas-renewal'], {'keep': True})
            self.assertEqual(declared['organs'], ['atlas-renewal', 'remote-seat-mail'])
            self.assertIn(str(control.workspace), declared['remote-seat-mail']['args'])
            self.assertIn(str(port), declared['remote-seat-mail']['args'])


class ObserverTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.journal = Journal(self.root)
        self.addCleanup(self.journal.close)

    def test_failed_mcp_inspection_is_reported_and_does_not_abort_other_observers(self):
        checks = {}
        with patch('real_seat_windows.api', side_effect=ValueError('untrusted source')):
            observe_mcp('http://127.0.0.1:1', self.root, self.journal, checks)
        self.assertFalse(checks['generated_mcp_config_trusted'][0])
        self.assertIn('untrusted source', self.journal.path.read_text())

    def test_subscription_cites_the_original_product_line_after_a_retry(self):
        lines = ['native mail unavailable session=ses_fixture stage=open cause=Deadline',
                 'native mail subscription opened session=ses_fixture']
        (self.root / 'product.stderr').write_text('\n'.join(lines) + '\n')
        checks = {}
        observe_subscription(self.root, 'ses_fixture', self.journal, checks)
        passed, index = checks['native_mail_subscribed']
        self.assertTrue(passed)
        row = self.journal.rows[index - 1][0]
        self.assertEqual(row['source_line'], 2)
        self.assertEqual(row['exact_line'], lines[1])

    def test_wait_cannot_escape_the_overall_deadline(self):
        control = Commands(self.root, self.root, self.root / 'node', self.journal, 0)
        with self.assertRaises(TimeoutError):
            control.wait(lambda: self.fail('expired wait must not observe'), 'expired')

    def test_initializer_readiness_cannot_be_used_for_the_new_service(self):
        path = self.root / 'serve-ready.json'
        child = Mock(pid=456)
        child.poll.return_value = None
        path.write_text('{"pid":123,"observed_endpoint":"http://127.0.0.1:1"}')
        self.assertIsNone(matching_readiness(self.root, child))
        path.write_text('{"pid":456,"observed_endpoint":"http://127.0.0.1:2"}')
        self.assertEqual(matching_readiness(self.root, child)['observed_endpoint'], 'http://127.0.0.1:2')

    def test_an_exited_product_cannot_reuse_its_old_readiness(self):
        child = Mock(pid=456)
        child.poll.return_value = 1
        with self.assertRaisesRegex(RuntimeError, 'exited before readiness'):
            matching_readiness(self.root, child)

    def test_product_output_bound_also_applies_while_waiting(self):
        control = Commands(self.root, self.root, self.root / 'node', self.journal, float('inf'))
        (self.root / 'product.stderr').write_bytes(b'x' * (2 * 1024 * 1024 + 1))
        with self.assertRaisesRegex(ValueError, 'exceeded 2 MiB'):
            control.wait(lambda: True, 'too noisy')


if __name__ == '__main__':
    unittest.main()
