"""Fail closed before directing input or cleanup at another console."""
from pathlib import Path
from copy import deepcopy
from types import SimpleNamespace
import hashlib
import json
import os
import tempfile
import unittest
from unittest.mock import Mock, patch

from windows_console import matches, release_empty_controller
from bound_mcp_model import require_acknowledged, require_continuity
from bundled_mcp_fixture import delivered_packet, Fixture
from windows_bound_mcp import isolated_environment
from real_seat_model import CONTINUITY


class Isolation(unittest.TestCase):
    def test_fleet_overrides_are_removed_and_bundled_se_precedes_fleet_path(self):
        root = Path('/private/disposable')
        payload = root / 'bundle'
        environment = isolated_environment(payload, root, dict(Path='/fleet/bin',
            STATEENGINE_PROJECT_ID='fleet', STATEENGINE_KEYS_DIR='/fleet/keys',
            GRAPT_CONFIG_DIR='/fleet/config', stateengine_registry_root='/fleet/registry',
            USERPROFILE='/fleet/home', TOKEN='private-secret'))
        self.assertEqual(environment['PATH'].split(os.pathsep)[0], str(payload))
        for key in ('STATEENGINE_PROJECT_ID', 'GRAPT_CONFIG_DIR', 'stateengine_registry_root', 'TOKEN'):
            self.assertNotIn(key, environment)
        for key in ('STATEENGINE_REGISTRY_ROOT', 'STATEENGINE_OWNER_ROOT', 'STATEENGINE_KEYS_DIR'):
            self.assertTrue(Path(environment[key]).is_relative_to(root / 'home'))

    def test_public_initializer_also_receives_the_explicit_private_registry(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            environment = isolated_environment(root / 'bundle', root, {})
            profile = dict(repo_root=str(root / 'home/project'),
                registry_root=environment['STATEENGINE_REGISTRY_ROOT'], project='private',
                agent_id='agt-private', human_id='human-private')
            reply = Mock(returncode=0, stdout=json.dumps({'profile': profile}).encode(), stderr=b'')
            with patch('bundled_mcp_fixture.subprocess.run', return_value=reply) as invoke:
                Fixture(root / 'bundle', environment)
            argv = invoke.call_args.args[0]
            self.assertEqual(argv[argv.index('--registry-root') + 1], environment['STATEENGINE_REGISTRY_ROOT'])


    def test_isolation_is_recorded_before_up_and_escaping_roots_refuse(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / 'home/project/.stateengine').mkdir(parents=True)
            environment = isolated_environment(root / 'bundle', root, {})
            profile = dict(repo_root=str(root / 'home/project'),
                registry_root=environment['STATEENGINE_REGISTRY_ROOT'], project='private',
                agent_id='agt-private', human_id='human-private')
            fixture = Fixture(root / 'bundle', environment, profile)
            def before_up(*args, **kwargs):
                evidence = json.loads((root / 'isolation-preflight.json').read_text())
                self.assertEqual(evidence['registry_root'], environment['STATEENGINE_REGISTRY_ROOT'])
                self.assertTrue(evidence['mail_endpoint'].startswith('http://127.0.0.1:'))
            fixture.cli = Mock(side_effect=before_up)
            with patch('bundled_mcp_fixture.sys.platform', 'win32'):
                fixture.start()
            fixture.cli.assert_called_once()
            for key in ('STATEENGINE_REGISTRY_ROOT', 'STATEENGINE_OWNER_ROOT', 'STATEENGINE_KEYS_DIR'):
                with self.subTest(key=key):
                    bad = Fixture(root / 'bundle', dict(environment, **{key: str(root / 'fleet')}), profile)
                    bad.cli = Mock()
                    with patch('bundled_mcp_fixture.sys.platform', 'win32'), self.assertRaises(RuntimeError):
                        bad.start()
                    bad.cli.assert_not_called()


class ConsoleIdentity(unittest.TestCase):
    def setUp(self):
        self.binary = Path('/private/grapt-tui.exe')
        self.profile = Path('/private/profile')
        self.session = 'ses_private_bound'
        self.row = dict(exe=str(self.binary), cmdline=[str(self.binary), '--service', str(self.profile),
            '--session', self.session])

    def test_only_exact_private_r_matches(self):
        self.assertTrue(matches(self.row, self.binary, self.profile, self.session))
        for key, value in [('exe', '/live/grapt-tui.exe'), ('cmdline', []),
                ('cmdline', [str(self.binary), '--service', '/live/profile', '--session', self.session]),
                ('cmdline', [str(self.binary), '--service', str(self.profile), '--session', 'ses_live']),
                ('cmdline', self.row['cmdline'] + ['--session', self.session]),
                ('cmdline', self.row['cmdline'][:-1])]:
            with self.subTest(key=key, value=value):
                self.assertFalse(matches(dict(self.row, **{key: value}), self.binary, self.profile, self.session))


class PacketContinuity(unittest.TestCase):
    def setUp(self):
        self.packet = 'a' * 64
        self.binding = dict(project='private-project', agent='agt-private')
        self.resume = dict(principal=dict(verified=True, project_id='private-project', agent_id='agt-private'),
            packet_acknowledgement=dict(state='acknowledged', recorded=True, packet_id=self.packet, acknowledgement_id='ack-private'))

    def body(self, packet=None, resumed=None):
        context = [{'launch_packet': 'packet_id: ' + (packet or self.packet)}, resumed or self.resume]
        return dict(input=[dict(role='user', content=CONTINUITY + '\n'.join(map(json.dumps, context)))])

    def test_missing_or_wrong_actual_delivery_refuses(self):
        require_continuity(self.body(), self.binding, self.packet)
        for body in ({'input': []}, self.body(packet='b' * 64)):
            with self.subTest(body=body), self.assertRaises((ValueError, RuntimeError)):
                require_continuity(body, self.binding, self.packet)

    def test_verified_principal_without_matching_recorded_ack_refuses(self):
        for key, value in [('state', 'no-delivery-recorded'), ('recorded', False), ('packet_id', 'b' * 64)]:
            resumed = deepcopy(self.resume)
            resumed['packet_acknowledgement'][key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                require_acknowledged(resumed, self.binding, self.packet)
            with self.subTest(first_request=key), self.assertRaises(RuntimeError):
                require_continuity(self.body(resumed=resumed), self.binding, self.packet)
        with self.assertRaises(RuntimeError):
            require_acknowledged(dict(principal=self.resume['principal']), self.binding, self.packet)

    def test_idempotent_resume_must_reference_the_original_recorded_ack(self):
        original = require_continuity(self.body(), self.binding, self.packet)
        repeated = deepcopy(self.resume)
        repeated['packet_acknowledgement']['recorded'] = False
        require_acknowledged(repeated, self.binding, self.packet, original)
        with self.assertRaises(RuntimeError):
            require_acknowledged(repeated, self.binding, self.packet)
        for flag in (None, 0, 1, 'false'):
            repeated['packet_acknowledgement']['recorded'] = flag
            with self.subTest(flag=flag), self.assertRaises(RuntimeError):
                require_acknowledged(repeated, self.binding, self.packet, original)
        repeated['packet_acknowledgement']['recorded'] = False
        for identity in ('different-ack', '', None):
            repeated['packet_acknowledgement']['acknowledgement_id'] = identity
            with self.subTest(identity=identity), self.assertRaises(RuntimeError):
                require_acknowledged(repeated, self.binding, self.packet, original)

    def test_packet_pointer_comes_from_public_session_instruction_not_launch_receipt(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            packet = root / 'packet.md'
            packet.write_text('packet_id: ' + self.packet)
            raw = packet.read_bytes()
            pointer = dict(state='written', path=str(packet), bytes=len(raw),
                           sha256=hashlib.sha256(raw).hexdigest())
            for entries in ([dict(key='stateengine-packet', value=pointer)], [],
                            [dict(key='stateengine-packet', value=pointer)] * 2):
                fixture = Fixture.__new__(Fixture)
                fixture.environment = {}
                fixture.profile = dict(project='private', repo_root=str(root), agent_id='agt-private')
                fixture.cli = Mock(return_value=dict(result='launched'))
                journey = SimpleNamespace(environment={}, root=root, profile=root,
                    args=SimpleNamespace(cli=root / 'grapt'),
                    api=Mock(side_effect=[dict(data=[dict(id='ses_private')]), dict(data=entries)]))
                if len(entries) == 1:
                    fixture.launch(journey, root / 'grapt-tui')
                    self.assertEqual(fixture.packet, self.packet)
                    journey.api.assert_called_with('GET', '/api/session/ses_private/instructions/entries')
                else:
                    with self.assertRaisesRegex(RuntimeError, 'unique delivered packet pointer'):
                        fixture.launch(journey, root / 'grapt-tui')

    def test_receipt_path_cannot_read_outside_private_workspace(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            workspace = root / 'workspace'
            workspace.mkdir()
            packet = root / 'foreign-packet.md'
            packet.write_text('packet_id: ' + self.packet)
            raw = packet.read_bytes()
            receipt = dict(grapt_packet=dict(state='written', path=str(packet), bytes=len(raw),
                                             sha256=hashlib.sha256(raw).hexdigest()))
            with patch.object(Path, 'read_bytes', side_effect=AssertionError('foreign bytes read')):
                with self.assertRaisesRegex(RuntimeError, 'escaped'):
                    delivered_packet(receipt['grapt_packet'], workspace)
            receipt['grapt_packet']['path'] = str(workspace / 'packet.md')
            packet.rename(workspace / 'packet.md')
            self.assertIn(self.packet, delivered_packet(receipt['grapt_packet'], workspace))
            receipt['grapt_packet']['sha256'] = 'b' * 64
            with self.assertRaisesRegex(RuntimeError, 'bytes differ'):
                delivered_packet(receipt['grapt_packet'], workspace)


class OwnedJobRelease(unittest.TestCase):
    @staticmethod
    def one_observation(read, description, seconds):
        if not read():
            raise RuntimeError('surviving owned child')

    def test_surviving_or_unverifiable_child_retains_lease(self):
        for members in ([os.getpid(), 999999], None):
            release = Mock()
            with patch('windows_console.wait', self.one_observation), patch('windows_console.job_members',
                    side_effect=RuntimeError('unverifiable census') if members is None else None,
                    return_value=members):
                with self.assertRaises(RuntimeError):
                    release_empty_controller(123, release)
            release.assert_not_called()

    def test_only_controller_releases_lease(self):
        release = Mock()
        with patch('windows_console.wait', self.one_observation), patch('windows_console.job_members',
                return_value=[os.getpid()]):
            release_empty_controller(123, release)
        release.assert_called_once_with()


if __name__ == '__main__':
    unittest.main()
