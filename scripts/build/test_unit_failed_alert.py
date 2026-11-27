"""A failed SE Code unit produces exactly one coordinator alert naming it.

The mail carries only what systemd passed the OnFailure handler as
MONITOR_* variables, so an unavailable journal, journal content, and a
later healthy run cannot change it. Witnesses: an exit-1 invocation sends
exactly one alert naming it and how to read it; a healthy invocation
sends none; the alert still sends when the journal is unavailable; the
reported invocation is the failed one even after a later success; the
body stays under one KiB. The installer wiring keeps that promise for
every timer-driven service, and only when a dedicated alert sender is
configured.
"""
import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import install_schedules
import nightly_revert
import unit_failed_alert

INVOKE = '0123456789abcdef0123456789abcdef'
UNIT = 'secode-retention.service'


def _env(result='exit', code='exit', status='1', invocation=INVOKE):
    env = {}
    if result is not None:
        env['MONITOR_SERVICE_RESULT'] = result
    if code is not None:
        env['MONITOR_EXIT_CODE'] = code
    if status is not None:
        env['MONITOR_EXIT_STATUS'] = status
    if invocation is not None:
        env['MONITOR_INVOCATION_ID'] = invocation
    return env


class AlertTests(unittest.TestCase):
    REGISTRY = ([{'agent_id': 'agt-alpha-1'}], 'agt-alpha-1')

    def _deliver(self, env, unit=UNIT, agent_id='secode-unit-failed'):
        sends = []

        def send(actor, recipient, body, *, timeout=None, idempotency_key=None):
            sends.append((actor, recipient, body, timeout))

        body = unit_failed_alert.deliver(unit, agent_id, env=env, send=send,
                                         registry=lambda: self.REGISTRY)
        return sends, body

    def test_failed_invocation_sends_exactly_one_naming_alert(self):
        sends, body = self._deliver(_env())
        self.assertEqual(len(sends), 1, 'exactly one mail per failed run')
        actor, recipient, sent, timeout = sends[0]
        self.assertEqual(actor, 'secode-unit-failed', 'its own sender, not a borrowed seat')
        self.assertEqual(recipient, 'agt-alpha-1', 'the coordinator')
        self.assertEqual(
            sent,
            'secode-unit-failed: SE Code unit secode-retention.service failed: '
            'result=exit, exit 1.\n'
            f'Invocation: {INVOKE}\n'
            f'Read locally: journalctl _SYSTEMD_INVOCATION_ID={INVOKE}',
            'only what systemd observed: no journal text can leak')
        self.assertTrue(sent.startswith('secode-unit-failed:'),
                        'the fixed prefix keeps the alert filterable')
        self.assertIs(body, sent)

    def test_alert_sends_when_the_journal_is_unavailable(self):
        # A journal that is unavailable must not lose the alert: the old
        # code raised before the send. There is no journal read now; an
        # empty PATH (no journalctl, no systemctl) must not matter.
        sent = []
        env = {key: value for key, value in os.environ.items()
               if not key.startswith('MONITOR_')}
        env.update(_env())
        env['PATH'] = str(Path(tempfile.mkdtemp(prefix='empty-bin-')))
        with patch.dict(os.environ, env, clear=True), \
                patch.object(nightly_revert, 'send',
                             side_effect=lambda actor, recipient, body, **kw:
                             sent.append((actor, recipient, body))), \
                patch.object(nightly_revert, 'registry', return_value=self.REGISTRY), \
                patch.object(sys, 'argv', ['unit_failed_alert.py',
                                           '--unit', UNIT,
                                           '--agent-id', 'secode-unit-failed']):
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = unit_failed_alert.main()
        self.assertEqual(code, 0)
        self.assertEqual(len(sent), 1, 'the alert is not lost to a missing journal')
        self.assertIn(f'Invocation: {INVOKE}', sent[0][2])
        self.assertIn(sent[0][2], stderr.getvalue(), 'the sent body is auditable')

    def test_failed_invocation_is_reported_after_a_later_success(self):
        # After a later run succeeded, the old code re-read live state,
        # saw Result=success, and sent nothing. The alert now depends
        # only on the failed trigger's MONITOR_* variables; the module
        # consults no live state at all.
        self.assertNotIn('subprocess', vars(unit_failed_alert),
                         'a later healthy run cannot change what is reported')
        sends, body = self._deliver(_env())
        self.assertEqual(len(sends), 1, 'the failed invocation is still reported')
        self.assertIn('result=exit, exit 1', sends[0][2])
        self.assertIn(f'Invocation: {INVOKE}', sends[0][2])

    def test_healthy_invocation_sends_nothing(self):
        sends, body = self._deliver(_env(result='success', code='success', status='0'))
        self.assertEqual(sends, [])
        self.assertIsNone(body)

    def test_missing_result_sends_nothing(self):
        sends, body = self._deliver(_env(result=None, code=None, status=None,
                                         invocation=None))
        self.assertEqual(sends, [])
        self.assertIsNone(body)

    def test_signal_failure_is_named(self):
        sends, body = self._deliver(_env(result='signal', code='signal', status='9'))
        self.assertEqual(len(sends), 1)
        self.assertIn('result=signal, signal 9', sends[0][2])

    def test_timeout_failure_has_no_exit(self):
        sends, body = self._deliver(_env(result='timeout', code=None, status=None))
        self.assertEqual(len(sends), 1)
        self.assertEqual(
            sends[0][2],
            f'secode-unit-failed: SE Code unit {UNIT} failed: result=timeout.\n'
            f'Invocation: {INVOKE}\n'
            f'Read locally: journalctl _SYSTEMD_INVOCATION_ID={INVOKE}')

    def test_body_stays_under_one_kib(self):
        unit = 'a' * 256
        env = _env(result='z' * 32, code='z' * 32, status='9' * 16)
        sends, body = self._deliver(env, unit=unit)
        self.assertEqual(len(sends), 1)
        self.assertLess(len(sends[0][2].encode('utf-8')), 1024)

    def test_malformed_monitor_value_refuses(self):
        for env in (_env(result='exit 1'), _env(invocation='x' * 33)):
            with self.assertRaises(ValueError):
                self._deliver(env)

    def test_send_failure_exits_one_and_reports(self):
        env = {key: value for key, value in os.environ.items()
               if not key.startswith('MONITOR_')}
        env.update(_env())
        with patch.dict(os.environ, env, clear=True), \
                patch.object(nightly_revert, 'send',
                             side_effect=RuntimeError('transport refused')), \
                patch.object(nightly_revert, 'registry', return_value=self.REGISTRY), \
                patch.object(sys, 'argv', ['unit_failed_alert.py',
                                           '--unit', UNIT,
                                           '--agent-id', 'secode-unit-failed']):
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = unit_failed_alert.main()
        self.assertEqual(code, 1)
        self.assertIn('could not be sent', stderr.getvalue())
        self.assertIn('transport refused', stderr.getvalue())

    def test_main_healthy_invocation_exits_zero_without_mail(self):
        env = {key: value for key, value in os.environ.items()
               if not key.startswith('MONITOR_')}
        env.update(_env(result='success', code='success', status='0'))
        sent = []
        with patch.dict(os.environ, env, clear=True), \
                patch.object(nightly_revert, 'send',
                             side_effect=lambda *a, **k: sent.append(1)), \
                patch.object(nightly_revert, 'registry', return_value=self.REGISTRY), \
                patch.object(sys, 'argv', ['unit_failed_alert.py',
                                           '--unit', UNIT,
                                           '--agent-id', 'secode-unit-failed']):
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = unit_failed_alert.main()
        self.assertEqual(code, 0)
        self.assertEqual(sent, [])
        self.assertEqual(stderr.getvalue(), '')


class InstallerTests(unittest.TestCase):
    def test_alert_identity_wires_every_timer_driven_service(self):
        specs = install_schedules.units(Path('/home/example/nightly'), Path('/home/example'),
                                        'agt-test-actor', Path('/home/example/lib'),
                                        alert_agent_id='secode-unit-failed')
        for name in ('secode-nightly.service', 'secode-disk-hygiene.service',
                     'secode-cleanup-alert.service', 'secode-disk-pressure.service',
                     'secode-job-history.service'):
            self.assertIn('OnFailure=secode-unit-failed@%n.service', specs[name])
        self.assertIn('--agent-id secode-unit-failed', specs['secode-cleanup-alert.service'],
                      'the cleaner alert speaks with its own sender')
        retention = install_schedules.retention_units(Path('/home/example'),
                                                      control=Path('/home/example/lib'),
                                                      alert_agent_id='secode-unit-failed')
        self.assertIn('OnFailure=secode-unit-failed@%n.service', retention['secode-retention.service'])
        self.assertIn('--unit %i --agent-id secode-unit-failed',
                      retention['secode-unit-failed@.service'])
        merged = {**specs, **retention}
        self.assertEqual(1, len([name for name in merged
                                 if name == 'secode-unit-failed@.service']),
                         'exactly one template')
        for name in ('secode-nightly.timer', 'secode-disk-hygiene.timer',
                     'secode-disk-pressure.timer', 'secode-job-history.timer',
                     'secode-cleanup-alert.timer', 'secodegate.slice'):
            self.assertNotIn('OnFailure=', specs[name])

    def test_legacy_install_keeps_today(self):
        specs = install_schedules.units(Path('/home/example/nightly'), Path('/home/example'),
                                        'agt-test-actor', Path('/home/example/lib'))
        for name, text in specs.items():
            self.assertNotIn('OnFailure=', text)
        self.assertNotIn('secode-unit-failed@.service', specs)
        self.assertIn('--agent-id agt-test-actor', specs['secode-cleanup-alert.service'])
        retention = install_schedules.retention_units(Path('/home/example'))
        self.assertNotIn('secode-unit-failed@.service', retention)
        self.assertNotIn('OnFailure=', retention['secode-retention.service'])

    def test_alert_identity_must_be_bounded(self):
        with self.assertRaises(ValueError):
            install_schedules.units(Path('/home/example/nightly'), Path('/home/example'),
                                    'agt-test-actor', alert_agent_id='not a seat id')
        with self.assertRaises(ValueError):
            install_schedules.retention_units(Path('/home/example'),
                                              alert_agent_id='x' * 200)
