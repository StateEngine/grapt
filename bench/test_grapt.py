"""Native ACP request failures retain their bounded, masked product explanation."""
import io
import json
from contextlib import redirect_stdout
from types import SimpleNamespace
import unittest

import grapt


class RequestErrors(unittest.TestCase):
    def request(self, response):
        child = SimpleNamespace(stdin=io.BytesIO(), stdout=io.BytesIO(
            (json.dumps({'jsonrpc': '2.0', 'id': 1, **response})+'\n').encode()))
        with redirect_stdout(io.StringIO()):
            return grapt.Client(child).request('session/prompt', {})

    def test_execution_failure_retains_product_message(self):
        message = 'The execution stopped before completing. Inspect the Session before retrying.'
        with self.assertRaisesRegex(RuntimeError, message):
            self.request({'error': {'code': -32603, 'message': message}})

    def test_missing_or_invalid_message_has_honest_fallback(self):
        for error in [None, {}, {'message': 7}, {'message': '  '}]:
            with self.subTest(error=error), self.assertRaisesRegex(RuntimeError, 'request refused: session/prompt'):
                self.request({'error': error})

    def test_error_does_not_expose_anthropic_key(self):
        with self.assertRaises(RuntimeError) as result:
            self.request({'error': {'message': 'credential sk-ant-test-secret rejected'}})
        self.assertNotIn('sk-ant-', str(result.exception))
        self.assertIn('[REDACTED]', str(result.exception))

    def test_error_is_one_bounded_line(self):
        with self.assertRaises(RuntimeError) as result:
            self.request({'error': {'message': 'cause\n\x1b'+ 'x'*1000}})
        message = str(result.exception)
        self.assertNotIn('\n', message)
        self.assertNotIn('\x1b', message)
        self.assertLessEqual(len(message), len('Native ACP request failed: session/prompt: ')+512)

    def test_success_retains_result(self):
        self.assertEqual(self.request({'result': {'stopReason': 'end_turn'}}), {'stopReason': 'end_turn'})


if __name__ == '__main__':
    unittest.main()
