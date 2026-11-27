"""Synthetic child tests only; no real proof, native transport or provider."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class PrivateConsumer(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="grapt-private-mail-")
        self.addCleanup(self.temporary.cleanup)
        self.owned = Path(self.temporary.name)
        self.owned.chmod(0o700)
        self.probe = self.owned / "probe.py"
        self.probe.write_text('''#!/usr/bin/python3
import json,sys
packet=json.load(sys.stdin)
if packet['client']=='refuse':
    sys.stderr.write('Error: ForeignReceipt '+packet['capability'])
    sys.exit(1)
identifier=packet['capability'] if packet['client']=='reflect' else 'joint-message'
rows=[{'source_message':identifier,'source_project':'original-origin','local_admission':1,
       'actionable':True,'delivery_pending':False,'repair_pending':False,'body':'PRIVATE_BODY'}]
if packet['client']=='missing': rows=[]
if packet['client']=='overflow': rows=rows*65
count=65 if packet['client']!='bad-count' else 9000
receipt={'live':True,'rows':rows,'shutdown':True,'admitted_count':count}
if packet.get('transport_protocol')=='remote-seat-mail-transport/v2':
    receipt.update(transport_protocol='remote-seat-mail-transport/v1' if packet['client']=='wrong-v2' else 'remote-seat-mail-transport/v2',integrity_gaps=6)
print(json.dumps(receipt))
''', encoding="utf-8")
        self.probe.chmod(0o700)
        self.root = Path(__file__).resolve().parents[2]
        self.script = Path(__file__).with_name("private_mail_consumer.py")

    def invoke(self, client="normal", proof=b"SYNTHETIC_PRIVATE_PROOF\n", selectors=None, protocol=None):
        command = [sys.executable, str(self.script), "--proof-line", "--recipient", "agt-demo", "--client", client,
                   "--probe", str(self.probe), "--owned", str(self.owned),
                   "--profile", str(self.root / "fixtures/stateengine-native/stateengine-mail-profile.json")]
        if protocol:
            command.extend(["--transport-protocol", protocol])
        for value in selectors or []:
            command.extend(["--witness-message", value])
        result = subprocess.run(command, input=proof, capture_output=True, timeout=10)
        self.assertNotIn(b"SYNTHETIC_PRIVATE_PROOF", result.stdout + result.stderr)
        self.assertNotIn(b"PRIVATE_BODY", result.stdout + result.stderr)
        self.assertFalse((self.owned / "mail.sqlite").exists())
        return result.returncode, json.loads(result.stdout)

    def test_selected_count_and_origin_are_public_facts(self):
        code, result = self.invoke(selectors=["joint-message"])
        self.assertEqual(code, 0)
        self.assertEqual(result["admitted_count"], 65)
        self.assertEqual(len(result["rows"]), 1)
        self.assertEqual(result["rows"][0]["source_project"], "original-origin")
        self.assertEqual(result["rows"][0]["body_utf8_bytes"], 12)

    def test_missing_selector_is_a_refusal(self):
        self.assertEqual(self.invoke(client="missing", selectors=["joint-message"])[0], 1)

    def test_output_overflow_and_count_overflow_refuse(self):
        for client in ["overflow", "bad-count"]:
            with self.subTest(client=client):
                self.assertEqual(self.invoke(client=client)[0], 1)

    def test_private_reflection_refuses(self):
        self.assertEqual(self.invoke(client="reflect")[0], 1)

    def test_closed_failure_has_no_private_diagnostic(self):
        code, result = self.invoke(client="refuse")
        self.assertEqual(code, 1)
        self.assertEqual(result["diagnostic_hints"], ["ForeignReceipt"])
        self.assertEqual(result["source_close"], "not established by output")

    def test_frame_and_selector_bounds_refuse(self):
        for proof in [b"unterminated", b"line\nline\n", b"has space\n", b"x" * 8193]:
            with self.subTest(proof_bytes=len(proof)):
                self.assertEqual(self.invoke(proof=proof)[0], 1)
        self.assertEqual(self.invoke(selectors=["joint-message"] * 9)[0], 1)


    def test_explicit_v2_keeps_body_private_and_reports_integrity_gaps(self):
        code, result = self.invoke(protocol="remote-seat-mail-transport/v2")
        self.assertEqual(code, 0)
        self.assertEqual(result["transport_protocol"], "remote-seat-mail-transport/v2")
        self.assertEqual(result["integrity_gaps"], 6)

    def test_v2_wrong_echo_is_not_a_v1_fallback(self):
        self.assertEqual(self.invoke(client="wrong-v2", protocol="remote-seat-mail-transport/v2")[0], 1)

if __name__ == "__main__":
    unittest.main()
