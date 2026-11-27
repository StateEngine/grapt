"""Exercise whole-driver refusals and the minimal current-pair fault controls."""
from contextlib import ExitStack
import hashlib
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import itertools
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import threading
import time
import unittest
from unittest.mock import Mock, patch

from real_seat_control import Commands
from real_seat_faults import LaunchProxy, job_command, report_control, selected_config
from real_seat_inputs import COMMAND, FEATURE, TARGET, verify_fixture
from real_seat_report import ASSERTIONS, Journal
from real_seat_windows import run


class FixtureTests(unittest.TestCase):
    def test_backend_bytes_are_bound_to_the_same_source_build_record(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            product = root / 'grapt-server.exe'
            product.write_bytes(b'fixture executable')
            receipt = dict(schema='grapt.seat.fixture-build.v1', source_commit='a' * 40,
                tree_dirty=False, target=TARGET, features=[FEATURE], command=COMMAND,
                outputs={'grapt-server.exe': dict(bytes=product.stat().st_size,
                    sha256=hashlib.sha256(product.read_bytes()).hexdigest())})
            path = root / 'build-receipt.json'
            path.write_text(json.dumps(receipt))
            self.assertEqual(verify_fixture(root, 'a' * 40), product)
            product.write_bytes(b'foreign executable')
            with self.assertRaisesRegex(ValueError, 'differs from its build record'):
                verify_fixture(root, 'a' * 40)
            product.write_bytes(b'fixture executable')
            for key, wrong in [('source_commit', 'b' * 40), ('tree_dirty', True),
                               ('features', []), ('target', 'foreign'), ('command', ['cargo'])]:
                path.write_text(json.dumps(dict(receipt, **{key: wrong})))
                with self.subTest(key=key), self.assertRaisesRegex(ValueError, 'same-source'):
                    verify_fixture(root, 'a' * 40)


class LaunchTests(unittest.TestCase):
    def exercise(self, exit_code, receipt_matches=True, stopped=None, alive=False):
        temporary = self.enterContext(tempfile.TemporaryDirectory())
        root = Path(temporary)
        profile = root / 'profile'
        profile.mkdir()
        generated = root / 'generated.json'
        control = Mock(root=root, workspace=root / 'workspace', registry=root / 'registry',
                       project='private', binding=['--project', 'private', '--root', str(root / 'workspace'),
                           '--registry-root', str(root / 'registry')], entry=['C:/Node Home/node.exe', 'E:/candidate/bin/se.mjs'],
                       environment={'SystemRoot': 'C:/Windows'})
        control.prepare_mcp_path.return_value = generated
        control.wait.side_effect = lambda read, *args, **kwargs: read()

        def public_command(name, arguments, *_args, **_kwargs):
            if name == 'init':
                return 0, json.dumps({'profile': {'agent_id': 'fixture', 'human_id': 'human'}})
            if name == 'launch':
                return exit_code, json.dumps({'grapt_mcp_config': str(generated if receipt_matches else root / 'foreign')})
            if name == 'send-mail':
                if '--root' in arguments or '--project-root' not in arguments:
                    raise ValueError('msg send requires project-root, not engine root')
                return 0, '{"msg_id":"msg-abc"}'
            if name == 'down':
                return 0, json.dumps(stopped if stopped is not None else dict(
                    action='stopped', project='private', session_pid=730001, organs=[],
                    orphans=[], stayed_up=[], shared_owner={'before': {'status': 'included'}},
                    identity_check={'status': 'verified'}))
            return 0, '{}'
        control.se.side_effect = public_command
        observed_commands = []

        def model(journal, _marker, command, **kwargs):
            observed_commands.append((command, kwargs))
            checks = {name: (True, journal.record(name, passed=True)) for name in ASSERTIONS}
            return SimpleNamespace(checks=checks, dispatch=Mock(), initial_idle=True, job_idle=True,
                                   serial=threading.Lock(), context={'job_notice': True})

        with ExitStack() as stack:
            replacements = dict(fresh_root=lambda _p: root, Commands=lambda *_a: control,
                declared_inputs=lambda *_a: ('a' * 40, root / 'product.exe'), SeatModel=model,
                prepare_profile=lambda *_a: profile, find_session=lambda *_a: 'ses_fixture',
                api=lambda *_a: {'data': {}}, observe_mcp=Mock(), observe_subscription=Mock(),
                observe_stability=Mock(),
                matching_readiness=lambda *_a: {'pid': 1, 'observed_endpoint': 'http://127.0.0.1:1'})
            for name, value in replacements.items():
                stack.enter_context(patch('real_seat_windows.' + name, value))
            provider = stack.enter_context(patch('real_seat_windows.Server'))
            provider.return_value.__enter__.return_value.endpoint = 'http://127.0.0.1:2'
            provider.return_value.__enter__.return_value.errors = []
            proxy = stack.enter_context(patch('real_seat_windows.LaunchProxy'))
            proxy.return_value.__enter__.return_value.endpoint = 'http://127.0.0.1:3'
            proxy.return_value.__enter__.return_value.applied = True
            proxy.return_value.__enter__.return_value.errors = []
            child = stack.enter_context(patch('real_seat_windows.subprocess.Popen'))
            child.return_value.returncode = 0
            args = SimpleNamespace(scratch=root, stateengine=root, node=root / 'node.exe',
                stateengine_commit='b' * 40, timeout_seconds=120, package=root, fault='none')
            # stop_engine imports psutil when it runs; a stand-in keeps the case off the host.
            stack.enter_context(patch.dict('sys.modules', {
                'psutil': SimpleNamespace(pid_exists=lambda _pid: alive)}))
            if alive:
                stack.enter_context(patch('real_seat_control.time.monotonic',
                                          side_effect=itertools.count(0, 60)))
            code = run(args)
        return code, json.loads((root / 'result.json').read_text()), observed_commands

    def test_failed_public_launch_cannot_pass_even_with_all_later_assertions_green(self):
        code, result, commands = self.exercise(1)
        self.assertEqual(code, 1)
        self.assertTrue(result['completed'])
        self.assertFalse(result['launch_confirmed'])
        self.assertFalse(result['passed'])
        self.assertIn("'C:/Node Home/node.exe' 'E:/candidate/bin/se.mjs' 'job' 'start' '--json' '--'", commands[0][0])

    def test_successful_launch_requires_its_actual_generated_config_receipt(self):
        code, result, _ = self.exercise(0, receipt_matches=False)
        self.assertEqual(code, 1)
        self.assertFalse(result['launch_confirmed'])

    def test_successful_launch_with_complete_evidence_can_pass(self):
        code, result, _ = self.exercise(0)
        self.assertEqual(code, 0)
        self.assertTrue(result['launch_confirmed'])
        self.assertTrue(result['passed'])

    def test_preserved_owner_cannot_be_reported_as_confirmed_cleanup(self):
        stopped = dict(action='stopped', project='private', session_pid=730001,
                       organs=[], orphans=[], stayed_up=[{'pid': 730002}],
                       shared_owner={'before': {'status': 'verified'}},
                       identity_check={'status': 'verified'})
        code, result, _ = self.exercise(0, stopped=stopped)
        self.assertEqual(code, 1)
        self.assertFalse(result['cleanup_confirmed'])
        code, result, _ = self.exercise(0, alive=True)
        self.assertEqual(code, 1)
        self.assertFalse(result['cleanup_confirmed'])


class FaultTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.journal = self.enterContext(Journal(self.root))
        self.control = Commands(self.root, self.root / 'candidate', self.root / 'node',
                                self.journal, time.monotonic() + 30)
        self.generated = self.root / 'generated.json'
        self.value = {'stateengine': {'native_mail_endpoint': 'http://127.0.0.1:49321'},
                      'mcp': {'stateengine': {'command': ['real-peer'], 'enabled': True}}}
        self.generated.write_text(json.dumps(self.value))

    def proxy(self, fault):
        selected = selected_config(self.control, self.generated, fault)
        proxy = LaunchProxy('http://127.0.0.1:1', self.control, self.generated, selected, fault)
        self.addCleanup(proxy.server_close)
        return proxy, selected

    def test_endpoint_control_removes_only_one_field_before_session_creation(self):
        proxy, _ = self.proxy('mail-endpoint-missing')
        proxy.faults('GET', '/api/session')
        self.assertFalse(proxy.applied)
        self.assertEqual(json.loads(self.generated.read_text()), self.value)
        self.assertEqual(proxy.faults('POST', '/api/session'), (False, False))
        self.assertTrue(proxy.applied)
        self.assertEqual(json.loads(self.generated.read_text()), {'stateengine': {}, 'mcp': self.value['mcp']})

    def test_trust_control_changes_only_the_config_location(self):
        proxy, selected = self.proxy('config-trust')
        self.assertTrue((selected.parent / '.git').is_dir())
        self.assertFalse(selected.exists())
        proxy.faults('POST', '/api/session')
        self.assertEqual(selected.read_bytes(), self.generated.read_bytes())
        self.assertEqual(self.control.environment['GRAPT_CONFIG'], str(selected))

    def test_positive_barrier_forwards_without_changing_generated_bytes(self):
        original = self.generated.read_bytes()
        proxy, selected = self.proxy('none')
        proxy.faults('POST', '/api/session')
        self.assertTrue(proxy.applied)
        self.assertEqual(self.generated.read_bytes(), original)
        self.assertEqual(selected, self.generated)

    def test_metadata_then_creation_cannot_bypass_the_fault_barrier(self):
        observed = []
        generated = self.generated
        class Product(BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'
            def log_message(self, *_args):
                pass
            def do_GET(self):
                observed.append((self.command, self.headers.get('Connection'),
                                 'native_mail_endpoint' in json.loads(generated.read_text())['stateengine']))
                self.send_response(200)
                self.send_header('Content-Length', '2')
                self.send_header('Connection', 'close')
                self.end_headers()
                self.wfile.write(b'{}')
            do_POST = do_GET
        product = ThreadingHTTPServer(('127.0.0.1', 0), Product)
        product.daemon_threads = True
        thread = threading.Thread(target=product.serve_forever, kwargs={'poll_interval': .01})
        thread.start()
        try:
            with LaunchProxy(f'http://127.0.0.1:{product.server_port}', self.control,
                             self.generated, self.generated, 'mail-endpoint-missing') as proxy:
                client = http.client.HTTPConnection(*proxy.server_address, timeout=3)
                try:
                    client.request('GET', '/api/session')
                    client.getresponse().read()
                    client.request('POST', '/api/session')
                    client.getresponse().read()
                finally:
                    client.close()
                self.assertTrue(proxy.applied)
                self.assertFalse(proxy.errors)
            self.assertEqual(observed, [('GET', 'close', True), ('POST', 'close', False)])
        finally:
            product.shutdown()
            product.server_close()
            thread.join(timeout=3)
            self.assertFalse(thread.is_alive())

    def test_job_control_changes_only_child_project_attribution(self):
        self.control.environment['SystemRoot'] = 'C:/Windows'
        normal, shell = job_command(self.control, 'C:/Python/python.exe', self.root / 'job.py', 'none')
        negative, selected_shell = job_command(self.control, 'C:/Python/python.exe', self.root / 'job.py',
                                               'job-project-missing')
        self.assertEqual(selected_shell, shell)
        self.assertEqual(negative, normal.replace(
            "$env:STATEENGINE_PROJECT_ID='" + self.control.project + "'; ",
            'Remove-Item Env:STATEENGINE_PROJECT_ID -ErrorAction Ignore; '))
        self.assertIn("$env:STATEENGINE_ROOT='" + str(self.control.workspace) + "'", normal)

    def test_unconfigured_is_not_a_successful_blocked_source_control(self):
        self.journal.record('launch_config_observed_before_provision', fault='config-trust')
        for status, expected in [('not_configured', False), ('blocked', True)]:
            line = self.journal.record('generated_mcp_config_trusted', passed=False,
                                       statuses=[{'status': status}])
            report_control(self.root, 'config-trust', self.journal,
                           {'generated_mcp_config_trusted': (False, line)})
            self.assertEqual(json.loads((self.root / 'control.json').read_text())['detected'], expected)

    def test_missing_owner_receipt_alone_does_not_prove_an_idle_no_wake_control(self):
        self.journal.record('launch_config_observed_before_provision', fault='job-project-missing')
        self.journal.record('job_started_by_seat', notice={
            'state': 'off', 'reason': 'no STATEENGINE_PROJECT_ID names the owning project'})
        line = self.journal.record('background_job_wakes_idle_seat', passed=False)
        checks = {'background_job_wakes_idle_seat': (False, line)}
        report_control(self.root, 'job-project-missing', self.journal, checks)
        self.assertFalse(json.loads((self.root / 'control.json').read_text())['detected'])
        self.journal.record('job_wake_absence_observed', idle_seconds=60)
        report_control(self.root, 'job-project-missing', self.journal, checks)
        self.assertTrue(json.loads((self.root / 'control.json').read_text())['detected'])


if __name__ == '__main__':
    unittest.main()
