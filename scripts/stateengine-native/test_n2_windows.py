"""The N2 journey must reject lost, replayed or altered producer input."""
import copy
import unittest

from headless_acceptance import SESSION
from n2_composer import verify_inputs
from sustained_provider import TASK, QUEUED, RESTART


def admissions():
    return [('session.input.admitted.1', dict(inputID='input_' + str(index),
        sessionID=SESSION, input=dict(delivery='queue', data=dict(text=text))))
        for index, text in enumerate([TASK, QUEUED, RESTART])]


class N2InputGuards(unittest.TestCase):
    def test_exact_edited_inputs_are_accepted(self):
        verify_inputs(admissions())

    def test_missing_input_is_not_a_pass(self):
        with self.assertRaisesRegex(RuntimeError, 'lost or replayed'):
            verify_inputs(admissions()[:-1])

    def test_replayed_identity_is_not_a_pass(self):
        rows = admissions()
        rows[2][1]['inputID'] = rows[0][1]['inputID']
        with self.assertRaisesRegex(RuntimeError, 'lost or replayed'):
            verify_inputs(rows)

    def test_unedited_draft_is_not_a_pass(self):
        rows = admissions()
        rows[1][1]['input']['data']['text'] += 'X'
        with self.assertRaisesRegex(RuntimeError, 'different text'):
            verify_inputs(rows)

    def test_another_session_or_delivery_route_is_not_a_pass(self):
        original = admissions()
        for other_session in (True, False):
            rows = copy.deepcopy(original)
            if other_session:
                rows[1][1]['sessionID'] = 'ses_another'
            else:
                rows[1][1]['input']['delivery'] = 'steer'
            with self.assertRaisesRegex(RuntimeError, 'different Session or delivery'):
                verify_inputs(rows)


if __name__ == '__main__':
    unittest.main()
