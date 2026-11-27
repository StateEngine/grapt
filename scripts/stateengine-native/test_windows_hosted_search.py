import unittest

from windows_hosted_search import observed_search


class HostedSearch(unittest.TestCase):
    def test_only_a_settled_native_call_and_successful_new_prompt_pass(self):
        receipt = dict(schema='grapt.cli.run-receipt.v1', session='ses_smoke',
                       prompted=True, cancelled=None, exit='success')
        tool = dict(type='tool', id='call_search', name='web_search', state=dict(status='completed'))
        history = dict(data=[dict(content=[tool])])
        self.assertEqual(observed_search([history, receipt], 0), 'ses_smoke')
        for records, code in [([receipt], 0), ([history, receipt], 1),
                              ([history, dict(receipt, prompted=False)], 0)]:
            with self.assertRaises(ValueError):
                observed_search(records, code)
        for status in ['running', 'error']:
            tool['state']['status'] = status
            with self.assertRaises(ValueError):
                observed_search([history, receipt], 0)
