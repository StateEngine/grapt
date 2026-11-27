"""Missing or invalid opaque producer inputs cannot become a skipped pass."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import bound_mcp_inputs as inputs


class RequiredProducerInputTests(unittest.TestCase):
    def test_missing_input_fails_before_any_producer_executes(self):
        with patch.object(subprocess, 'Popen') as execute:
            with self.assertRaisesRegex(ValueError, 'input missing: SECODE_STATEENGINE_PAYLOAD'):
                inputs.required_payload({})
            execute.assert_not_called()

    def test_real_entrypoint_refuses_missing_input_with_nonzero_status(self):
        environment = dict(os.environ)
        environment.pop('SECODE_STATEENGINE_PAYLOAD', None)
        result = subprocess.run([sys.executable, inputs.__file__], env=environment,
                                capture_output=True, text=True, timeout=15, check=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn('input missing: SECODE_STATEENGINE_PAYLOAD', result.stderr)
        self.assertNotIn('producer-input-verified', result.stdout)

    def test_relative_and_absent_directories_are_named_failures(self):
        for value in ('relative', '/absent-bound-mcp-payload'):
            with self.subTest(value=value), self.assertRaisesRegex(ValueError, 'absolute payload directory'):
                inputs.required_payload({'SECODE_STATEENGINE_PAYLOAD': value})

    def test_real_pin_is_required_before_existing_payload_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            pins = root / 'packages/stateengine/payload-pins.json'
            pins.parent.mkdir(parents=True)
            pins.write_text(json.dumps({'schema': inputs.stateengine_payload.PIN_SCHEMA, 'targets': {}}))
            with patch.object(inputs.stateengine_payload, 'verify') as verify:
                with self.assertRaisesRegex(ValueError, 'producer pin missing'):
                    inputs.required_payload({'SECODE_STATEENGINE_PAYLOAD': directory}, root)
                verify.assert_not_called()

    def test_current_host_pin_is_verified_and_verification_errors_propagate(self):
        for host, target in [('linux', 'x86_64-unknown-linux-gnu'),
                             ('win32', 'x86_64-pc-windows-msvc')]:
            with self.subTest(host=host), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                pins = root / 'packages/stateengine/payload-pins.json'
                pins.parent.mkdir(parents=True)
                pin = {'source_commit': 'a' * 40, 'version': 'fixture'}
                pins.write_text(json.dumps({'schema': inputs.stateengine_payload.PIN_SCHEMA,
                                            'targets': {target: pin}}))
                with patch.object(inputs.sys, 'platform', host), \
                        patch.object(inputs.stateengine_payload, 'verify', return_value=({'target': target}, {})) as verify:
                    self.assertEqual(inputs.required_payload({'SECODE_STATEENGINE_PAYLOAD': directory}, root),
                                     (root, {'target': target}))
                    verify.assert_called_once_with(root, target, pin)
                with patch.object(inputs.sys, 'platform', host), \
                        patch.object(inputs.stateengine_payload, 'verify',
                                     side_effect=inputs.stateengine_payload.PayloadError('attestation differs')):
                    with self.assertRaisesRegex(inputs.stateengine_payload.PayloadError, 'attestation differs'):
                        inputs.required_payload({'SECODE_STATEENGINE_PAYLOAD': directory}, root)


if __name__ == '__main__':
    unittest.main()
