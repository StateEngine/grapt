import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import accounts
import grapt


class AccountTests(unittest.TestCase):
    @unittest.skipUnless(hasattr(os, 'getuid') and hasattr(os, 'O_NOFOLLOW'), 'Unix ownership guard')
    def test_managed_codex_link_aligns_without_copying_or_changing_the_link(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            target = root/'managed.json'
            target.write_text(json.dumps({'tokens': {'account_id': 'synthetic-account'}}))
            target.chmod(0o600)
            codex = root/'codex.json'
            codex.symlink_to(target)
            opencode = root/'opencode.json'
            opencode.write_text(json.dumps({'openai': {'type': 'oauth', 'accountId': 'synthetic-account'}}))
            with patch.object(Path, 'home', return_value=root), patch.object(accounts, 'fetch') as fetch, \
                    patch.object(accounts.os.path, 'realpath', wraps=os.path.realpath) as resolve:
                self.assertEqual(accounts.aligned(codex, opencode)['account_id'], 'synthetic-account')
                fetch.assert_not_called()
                resolve.assert_called_once_with(codex, strict=True)
            self.assertTrue(codex.is_symlink())
            self.assertEqual(codex.readlink(), target)
            self.assertEqual({path.name for path in root.iterdir()},
                             {'managed.json', 'codex.json', 'opencode.json'})
            # The exception belongs only to Codex, not the generic document reader.
            with self.assertRaisesRegex(ValueError, 'bounded regular file'):
                accounts.document(codex)
            with patch.object(Path, 'home', return_value=root):
                with self.assertRaisesRegex(ValueError, 'bounded regular file'):
                    accounts.aligned(target, codex)

    @unittest.skipUnless(hasattr(os, 'getuid') and hasattr(os, 'O_NOFOLLOW'), 'Unix ownership guard')
    def test_managed_codex_link_refuses_writable_foreign_and_outside_targets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            home = root/'home'
            home.mkdir()
            target = home/'managed.json'
            target.write_text('{}')
            target.chmod(0o600)
            codex = home/'codex.json'
            codex.symlink_to(target)
            with patch.object(Path, 'home', return_value=home):
                for mode in [0o620, 0o602]:
                    target.chmod(mode)
                    with self.assertRaisesRegex(ValueError, 'group- or world-writable'):
                        accounts.codex_document(codex)
                target.chmod(0o600)
                real_fstat = os.fstat
                def foreign(descriptor):
                    fields = list(real_fstat(descriptor))
                    fields[4] = os.getuid() + 1
                    return os.stat_result(fields)
                with patch.object(accounts.os, 'fstat', side_effect=foreign):
                    with self.assertRaisesRegex(ValueError, 'foreign uid'):
                        accounts.codex_document(codex)
                codex.unlink()
                outside = root/'outside.json'
                outside.write_text('{}')
                outside.chmod(0o600)
                codex.symlink_to(outside)
                with self.assertRaisesRegex(ValueError, 'outside the home'):
                    accounts.codex_document(codex)

    @unittest.skipUnless(hasattr(os, 'getuid') and hasattr(os, 'O_NOFOLLOW'), 'Unix ownership guard')
    def test_managed_codex_link_refuses_a_parent_swapped_after_resolution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            home, outside = root/'home', root/'outside'
            managed = home/'managed'
            managed.mkdir(parents=True)
            outside.mkdir()
            target = managed/'auth.json'
            for file in [target, outside/'auth.json']:
                file.write_text('{}')
                file.chmod(0o600)
            codex = home/'codex.json'
            codex.symlink_to(target)
            original_open = os.open
            swapped = False
            def swap(path, flags, *args, **kwargs):
                nonlocal swapped
                # Exercise both the old absolute open and the anchored walk.
                if not swapped and (Path(path) == target or path == 'managed'):
                    managed.rename(home/'retained')
                    managed.symlink_to(outside, target_is_directory=True)
                    swapped = True
                return original_open(path, flags, *args, **kwargs)
            with patch.object(Path, 'home', return_value=home), \
                    patch.object(accounts.os, 'open', side_effect=swap):
                with self.assertRaisesRegex(ValueError, 'unreadable'):
                    accounts.codex_document(codex)
            self.assertTrue(swapped, 'the parent-swap witness did not run')

    @unittest.skipUnless(hasattr(os, 'getuid') and hasattr(os, 'O_NOFOLLOW'), 'Unix ownership guard')
    def test_managed_codex_link_keeps_file_size_and_json_bounds(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            target = root/'managed.json'
            codex = root/'codex.json'
            codex.symlink_to(target)
            with patch.object(Path, 'home', return_value=root):
                target.mkdir()
                with self.assertRaisesRegex(ValueError, 'not a regular file|unreadable'):
                    accounts.codex_document(codex)
                target.rmdir()
                target.write_text('{}')
                target.chmod(0o600)
                self.assertEqual(accounts.codex_document(codex, maximum=2), {})
                with self.assertRaisesRegex(ValueError, 'document bound'):
                    accounts.codex_document(codex, maximum=1)
                for raw, reason in [('bad', 'invalid'), ('[]', 'must be an object')]:
                    target.write_text(raw)
                    with self.assertRaisesRegex(ValueError, reason):
                        accounts.codex_document(codex)

    def inputs(self, root, paid=False):
        c, o, key = root/'codex.json', root/'opencode.json', root/'key'
        c.write_text(json.dumps({'tokens': {'account_id': 'synthetic-account', 'access_token': 'synthetic-token'}}))
        o.write_text(json.dumps({'openai': {'type': 'oauth', 'accountId': 'synthetic-account'}}))
        key.write_text('synthetic-paid-key'); key.chmod(0o600)
        responses = [{'models': [{'slug': 'gpt-6-luna', 'context_window': 272000}]}]
        if paid:
            responses.append({'data': [{'id': accounts.HAIKU, 'name': 'Haiku',
                'supported_parameters': ['tools'], 'context_length': 1000000,
                'top_provider': {'max_completion_tokens': 128000}}]})
        with patch.object(accounts, 'fetch', side_effect=responses):
            accounts.prepare(root/'private', c, o, key, include_paid=paid)
        return c, o

    def test_execution_freezes_matching_inputs_and_refuses_account_or_token_rotation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); c, o = self.inputs(root)
            source = root/'private/luna'
            staged = accounts.execution_inputs(source, root/'selected', 'luna', c, o)
            self.assertEqual((staged/'provider.credential').read_text(), 'synthetic-token')
            config_path = source/'opencode.jsonc'
            config = json.loads(config_path.read_text()); config['provider']['account'] = 'other-account'
            config_path.write_text(json.dumps(config))
            with self.assertRaisesRegex(ValueError, 'differs') as error:
                accounts.execution_inputs(source, root/'mismatch', 'luna', c, o)
            self.assertNotIn('other-account', str(error.exception))
            self.assertFalse((root/'mismatch').exists())
            config['provider']['account'] = 'synthetic-account'; config_path.write_text(json.dumps(config))
            c.write_text(json.dumps({'tokens': {'account_id': 'synthetic-account', 'access_token': 'rotated-token'}}))
            with self.assertRaisesRegex(ValueError, 'differs'):
                accounts.execution_inputs(source, root/'rotated', 'luna', c, o)
            self.assertEqual((staged/'provider.credential').read_text(), 'synthetic-token')

    def test_execution_paid_key_and_route_must_match_the_budget(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); c, o = self.inputs(root, paid=True)
            source = root/'private/haiku'
            accounts.execution_inputs(source, root/'selected', 'haiku', c, o, 'synthetic-paid-key')
            with self.assertRaisesRegex(ValueError, 'budget-monitored') as error:
                accounts.execution_inputs(source, root/'rotated', 'haiku', c, o, 'rotated-key')
            self.assertNotIn('rotated-key', str(error.exception))
            self.assertFalse((root/'rotated').exists())
            path = source/'opencode.jsonc'
            config = json.loads(path.read_text()); config['provider']['accounts'][0]['endpoint'] = 'https://wrong.invalid'
            path.write_text(json.dumps(config))
            with self.assertRaisesRegex(ValueError, 'route'):
                accounts.execution_inputs(source, root/'wrong-route', 'haiku', c, o, 'synthetic-paid-key')
            self.assertFalse((root/'wrong-route').exists())

    def test_haiku_plan_keeps_routes_and_rejects_historical_sonnet_inputs(self):
        adapters = json.loads(Path(accounts.__file__).with_name('adapters.json').read_text())
        self.assertEqual({key for key, value in adapters.items() if value['cohort'] == 'haiku'},
                         {'claude', 'opencode-haiku', 'grapt-haiku'})
        for key in ['opencode-haiku', 'grapt-haiku']:
            self.assertEqual(adapters[key]['auth'], 'openrouter')
            self.assertEqual(adapters[key]['model'], 'openrouter/' + accounts.HAIKU)
        self.assertEqual(adapters['claude']['auth'], 'claude-max')
        self.assertEqual(adapters['claude']['model'], 'claude-haiku-5-5')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); c, o = self.inputs(root, paid=True)
            source = root/'private/haiku'
            model = json.loads((source/'openrouter-models.json').read_text())['models'][0]
            self.assertEqual(model['slug'], accounts.HAIKU)
            self.assertEqual(model['context_window'], 1000000)
            catalog = json.loads((source/'model-catalog.json').read_text())
            self.assertEqual(catalog['openrouter']['models'][accounts.HAIKU]['limit'],
                             {'context': 1000000, 'input': 1000000, 'output': 128000})
            with self.assertRaisesRegex(ValueError, 'Unsupported benchmark account cohort'):
                accounts.execution_inputs(source, root/'old', 'sonnet', c, o, 'synthetic-paid-key')
            self.assertFalse((root/'old').exists())

    def test_execution_accepts_product_sized_inventory_but_bounds_secrets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); c, o = self.inputs(root)
            source = root/'private/luna'
            inventory = source/'account-models.json'
            contents = inventory.read_text() + '\n' * 80000
            inventory.write_text(contents)
            staged = accounts.execution_inputs(source, root/'selected', 'luna', c, o)
            self.assertEqual((staged/'account-models.json').read_text(), contents)
            inventory.write_text(' ' * (16 * 1024 * 1024 + 1))
            with self.assertRaisesRegex(ValueError, 'bounded private regular file'):
                accounts.execution_inputs(source, root/'oversized-inventory', 'luna', c, o)
            self.assertFalse((root/'oversized-inventory').exists())
            inventory.write_text(contents)
            (source/'provider.credential').write_text('x' * 65537)
            with self.assertRaisesRegex(ValueError, 'bounded private regular file'):
                accounts.execution_inputs(source, root/'oversized-secret', 'luna', c, o)
            self.assertFalse((root/'oversized-secret').exists())

    def test_mismatched_account_refuses_before_discovery_or_file_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            codex, opencode = root/'codex.json', root/'opencode.json'
            codex.write_text(json.dumps({'tokens': {'account_id': 'one', 'access_token': 'secret'}}))
            opencode.write_text(json.dumps({'openai': {'type': 'oauth', 'accountId': 'two'}}))
            with patch.object(accounts, 'fetch') as fetch:
                with self.assertRaisesRegex(ValueError, 'do not match'):
                    accounts.prepare(root/'private', codex, opencode, root/'key')
                fetch.assert_not_called()
                self.assertFalse((root/'private').exists())

    def test_private_luna_import_has_no_database_and_no_secret_in_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); c, o = root/'codex.json', root/'opencode.json'
            c.write_text(json.dumps({'tokens': {'account_id': 'one', 'access_token': 'private-sentinel',
                'refresh_token': 'never-import-refresh-sentinel', 'id_token': 'never-import-id-sentinel'}}))
            o.write_text(json.dumps({'openai': {'type': 'oauth', 'accountId': 'one'}}))
            with patch.object(accounts, 'fetch', return_value={'models': [
                    {'slug': 'gpt-6-luna', 'visibility': 'list', 'context_window': 272000,
                     'supported_reasoning_levels': []}]}):
                receipt = accounts.prepare(root/'private', c, o, root/'key', include_paid=False)
            self.assertNotIn('private-sentinel', json.dumps(receipt))
            self.assertFalse((root/'private/luna/database.sqlite3').exists())
            self.assertEqual((root/'private').stat().st_mode & 0o777, 0o700)
            self.assertEqual((root/'private/luna/provider.credential').read_text(), 'private-sentinel')
            for file in (root/'private/luna').iterdir():
                self.assertEqual(file.stat().st_mode & 0o777, 0o600)
                self.assertNotIn('never-import-refresh-sentinel', file.read_text())
                self.assertNotIn('never-import-id-sentinel', file.read_text())

    def test_acp_permission_callback_uses_allow_once(self):
        # A finite fake native ACP server requests permission before the reply.
        code = """import sys,json
r=json.loads(sys.stdin.readline())
print(json.dumps({'jsonrpc':'2.0','id':99,'method':'session/request_permission','params':{'options':[{'kind':'allow_once','optionId':'once'}]}}),flush=True)
a=json.loads(sys.stdin.readline())
assert a['result']['outcome']['optionId']=='once'
print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':{'ok':True}}),flush=True)
"""
        child = subprocess.Popen([sys.executable, '-c', code], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        try:
            with patch('sys.stdout', io.StringIO()):
                result = grapt.Client(child).request('initialize', {})
            self.assertEqual(result, {'ok': True})
            child.wait(timeout=2)
        finally:
            child.stdin.close(); child.stdout.close()
            if child.poll() is None: child.kill(); child.wait(timeout=2)
