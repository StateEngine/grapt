"""Reject false integration passes for altered input or mismatched approvals."""
import copy
import unittest

from headless_acceptance import SESSION
from r_sustained_walk import verify_approvals, verify_inputs
from sustained_provider import TASK, QUEUED, RESTART
from r_reliability_provider import LOST, RECOVER
from r_recovery_journey import verify_recovery


def admissions():
    return [{'inputID': f'msg_fixture{n}', 'sessionID': SESSION,
             'input': {'type': 'user', 'delivery': 'queue', 'data': {'text': text}}}
            for n, text in enumerate((TASK, QUEUED, RESTART, LOST, RECOVER))]


def approvals():
    decisions = {f'per_fixture{n}': action for n, action in
                 enumerate(('patch', 'shell', 'patch', 'patch', 'shell', 'shell', 'shell', 'shell'))}
    rows = [(kind, value) for identity, action in decisions.items() for kind, value in (
        ('permission.asked.2', {'id': identity, 'action': action}),
        ('permission.replied.2', {'requestID': identity, 'reply': 'once', 'sessionID': SESSION}))]
    return rows, decisions


class RIntegrationEvidenceTests(unittest.TestCase):
    def test_complete_composer_and_approval_evidence_is_accepted(self):
        verify_inputs(admissions())
        verify_approvals(*approvals())

    def test_unedited_suffix_cannot_pass_as_the_queued_input(self):
        rows = admissions()
        rows[1]['input']['data']['text'] += 'X'
        with self.assertRaisesRegex(RuntimeError, 'edited composer input'):
            verify_inputs(rows)

    def test_replayed_identity_cannot_replace_a_lost_input(self):
        rows = admissions()
        rows[2]['inputID'] = rows[0]['inputID']
        with self.assertRaisesRegex(RuntimeError, 'lost or replayed'):
            verify_inputs(rows)

    def test_cli_steering_cannot_stand_in_for_composer_queueing(self):
        rows = admissions()
        rows[1]['input']['delivery'] = 'steer'
        with self.assertRaisesRegex(RuntimeError, 'wrong producer input route'):
            verify_inputs(rows)

    def test_other_request_reply_cannot_satisfy_an_approval(self):
        rows, decisions = approvals()
        rows[1][1]['requestID'] = 'per_other'
        with self.assertRaisesRegex(RuntimeError, 'approval identity'):
            verify_approvals(rows, decisions)

    def test_saved_policy_cannot_replace_once_in_the_real_ui(self):
        rows, decisions = approvals()
        rows[1][1]['reply'] = 'always'
        with self.assertRaisesRegex(RuntimeError, 'approval identity or decision'):
            verify_approvals(rows, decisions)

    def test_unrendered_approval_cannot_pass_from_backend_events_alone(self):
        rows, decisions = approvals()
        decisions = copy.copy(decisions)
        decisions.pop(next(iter(decisions)))
        with self.assertRaisesRegex(RuntimeError, 'missing or unexpected'):
            verify_approvals(rows, decisions)

    def test_lost_effect_requires_one_matching_terminal_and_only_the_injected_failure(self):
        rows = [
            ('session.tool.nested.started.1', dict(call='call_lost', attempt='attempt_lost',
                parent='call_sustain_lost_parent')),
            ('session.tool.terminal.1', dict(call='call_lost', attempt='attempt_lost',
                outcome=dict(reason='effect-unknown'))),
            ('session.step.failed.2', dict(reason=dict(kind='process-loss'))),
        ]
        verify_recovery(rows)
        for field in ['call', 'attempt']:
            wrong = copy.deepcopy(rows)
            wrong[1][1][field] = 'another_identity'
            with self.assertRaisesRegex(RuntimeError, 'another identity'):
                verify_recovery(wrong)
        with self.assertRaisesRegex(RuntimeError, 'more than once'):
            verify_recovery([*rows, rows[1]])
        with self.assertRaisesRegex(RuntimeError, 'unexpected failure'):
            verify_recovery([*rows, ('session.step.failed.3', dict(reason=dict(kind='provider-failure')))])


if __name__ == '__main__':
    unittest.main()
