"""Carrier limits must not silently become native-seat or journey passes."""
import json
import io
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from headless_acceptance import SESSION
from ordinary_profile import OrdinaryProfile
from r_windows_reliability import Steps, STEPS
from r_windows_terminal import WindowsTerminal, native_session


class WindowsCarrierTests(unittest.TestCase):
    def test_native_session_requires_a_real_ready_process_identity(self):
        started=dict(started=True, transport='Windows ConPTY')
        for pid in [None, 0, -1, True, '42']:
            with self.subTest(pid=pid), self.assertRaisesRegex(RuntimeError, 'native R process identity'):
                native_session(dict(started, pid=pid))
        self.assertEqual(native_session(dict(started, pid=42)), '42')

    def test_control_replies_cannot_switch_to_another_r_process(self):
        terminal=object.__new__(WindowsTerminal)
        terminal.session='42'
        terminal.args=SimpleNamespace(record=lambda *_:None)
        terminal.observer=SimpleNamespace(stdin=io.BytesIO())
        terminal.reply=lambda:dict(pid=43, screen='Connected')
        with self.assertRaisesRegex(RuntimeError, 'replaced the original R process'):
            terminal.request('screen')
        terminal.reply=lambda:dict(pid=42, screen='Connected')
        self.assertEqual(terminal.request('screen')['screen'], 'Connected')

    def test_failed_phase_remains_failed_when_finally_writes_the_table_again(self):
        with tempfile.TemporaryDirectory() as root:
            steps=Steps(Path(root))
            steps.checkpoint(STEPS[0])
            steps.finish(RuntimeError('injected lost approval'))
            steps.finish()
            rows=json.loads((Path(root)/'steps.json').read_text())
            self.assertEqual([row['state'] for row in rows[:3]], ['PASS','FAIL','NOT RUN'])
            self.assertEqual(rows[1]['error'], 'injected lost approval')

    def test_omitting_an_assertion_cannot_advance_to_a_later_phase(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaisesRegex(RuntimeError, 'unexpected journey phase'):
                Steps(Path(root)).checkpoint(STEPS[1])

    def test_session_only_setup_never_provisions_a_native_identity_or_capability(self):
        profile=object.__new__(OrdinaryProfile)
        profile.args=SimpleNamespace(session_only=True)
        profile.profile=Path('fixture')
        calls=[]
        def api(method, route, body):
            calls.append((method,route,body))
            return {'data': {'id':SESSION}}
        profile.api=api
        self.assertEqual(profile.provision().returncode, 0)
        profile.rebind()
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][:2], ('POST','/api/session'))
        self.assertEqual(set(calls[0][2]), {'id','agent','model','location'})


if __name__=='__main__':
    unittest.main()
