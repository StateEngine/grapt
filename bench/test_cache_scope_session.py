import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import cache_scope_session as scope


class CacheScopeSessions(unittest.TestCase):
    def run_probe(self, projects=None, repeated=False, answer='READY', initialize_only=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'README.md').write_text('fixture\n')
            config = root/'service.conf'
            original = 'other=value\nprompt_cache_scope=session\n'
            config.write_text(original)
            capture = scope.Capture(io.StringIO())

            class Client:
                def __init__(self):
                    self.sessions, self.prompts, self.initializations = 0, [], 0

                def request(self, method, params):
                    if method == 'initialize':
                        self.initializations += 1
                    if method == 'session/new':
                        self.sessions += 1
                        return {'sessionId': 'ses_same' if repeated else 'ses_'+str(self.sessions)}
                    return {}

                def prompt(self, session, text):
                    self.prompts.append((session, config.read_text(), text))
                    capture.answer.append(answer)
                    return {'stopReason': 'end_turn'}

            client = Client()
            def location(database, session):
                return {'project': (projects or ['shared']*4)[client.sessions-1], 'directory': str(root)}

            with patch.object(Path, 'cwd', return_value=root), patch.object(scope, 'placement', side_effect=location), \
                    patch.object(scope, 'collect', return_value={'call_usage': None, 'usage': None}), \
                    patch.object(Path, 'read_text', autospec=True, side_effect=lambda p, *a, **kw:
                                 'some avg10=0.00\n' if str(p) == '/proc/pressure/io' else original_read(p, *a, **kw)):
                result = scope.probe(client, capture, root, 30, initialize_only)
            self.assertEqual(config.read_text(), original)
            return result, client

    def test_four_fresh_sessions_keep_one_owner_and_shared_project(self):
        result, client = self.run_probe()
        self.assertTrue(result['success'])
        self.assertEqual(client.initializations, 1)
        self.assertEqual(len(client.prompts), 4)
        self.assertEqual(len({row['session'] for row in result['rows']}), 4)
        self.assertEqual(len({row['expected_body_key'] for row in result['rows']}), 1)
        for (mode, _), (_, config, prompt) in zip(scope.ORDER, client.prompts):
            self.assertIn('prompt_cache_scope='+mode+'\n', config)
            self.assertEqual(prompt, scope.PROMPT)
        self.assertNotEqual(result['rows'][0]['expected_session_header'], result['rows'][0]['expected_body_key'])
        self.assertEqual(result['rows'][2]['expected_session_header'], result['rows'][2]['expected_body_key'])
        self.assertIsNone(result['rows'][0]['single_call'])  # Absent usage remains unknown.

    def test_changed_project_stops_before_second_prompt(self):
        result, client = self.run_probe(projects=['one', 'two', 'one', 'one'])
        self.assertFalse(result['success'])
        self.assertEqual(len(client.prompts), 1)
        self.assertIn('ProjectId changed', result['error'])
        self.assertEqual(result['rows'][2]['state'], 'not-dispatched')

    def test_reused_session_stops_before_second_prompt(self):
        result, client = self.run_probe(repeated=True)
        self.assertFalse(result['success'])
        self.assertEqual(len(client.prompts), 1)
        self.assertIn('Session was reused', result['error'])

    def test_failed_smoke_skips_its_pair_without_retrying(self):
        result, client = self.run_probe(answer='WRONG')
        self.assertFalse(result['success'])
        self.assertEqual(len(client.prompts), 2)
        self.assertEqual([row['state'] for row in result['rows']],
                         ['prompt-dispatched', 'not-dispatched', 'prompt-dispatched', 'not-dispatched'])

    def test_initialize_only_creates_four_sessions_without_prompts(self):
        result, client = self.run_probe(initialize_only=True)
        self.assertTrue(result['success'])
        self.assertEqual(client.sessions, 4)
        self.assertEqual(client.prompts, [])

    def test_plan_does_not_construct_a_client(self):
        with patch('sys.argv', ['cache_scope_session.py']), patch.object(scope, 'NativeSession') as native, \
                patch('sys.stdout', new_callable=io.StringIO):
            self.assertEqual(scope.main(), 0)
            native.assert_not_called()

    def test_foreign_session_chunks_cannot_supply_the_ready_answer(self):
        capture = scope.Capture(io.StringIO())
        capture.session = 'ses_selected'
        capture.write(json.dumps({'direction': 'server', 'message': {'params': {
            'sessionId': 'ses_foreign', 'update': {'sessionUpdate': 'agent_message_chunk',
                                                 'content': {'text': 'READY'}}}}}))
        self.assertEqual(capture.answer, [])

    def test_answer_bound_fails_loudly(self):
        capture = scope.Capture(io.StringIO())
        capture.session = 'ses_selected'
        with self.assertRaisesRegex(ValueError, 'exceeds its bound'):
            capture.write(json.dumps({'direction': 'server', 'message': {'params': {
                'sessionId': capture.session, 'update': {'sessionUpdate': 'agent_message_chunk',
                    'content': {'text': 'x'*(scope.MAX_REPLY_BYTES+1)}}}}}))


original_read = Path.read_text

if __name__ == '__main__':
    unittest.main()
