"""Machine receipt consumers opt into JSON independently of StateEngine defaults."""
import ast
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import unittest
from unittest.mock import patch

from bundled_mcp_fixture import Fixture

ROOT = Path(__file__).resolve().parents[2]


def literal_flags(arguments):
    return [item.value for item in arguments.elts if isinstance(item, ast.Constant)
            and isinstance(item.value, str)]


def missing_json(source):
    """Cover lifecycle callers and both direct/generated bundled CLI argv owners."""
    missing = []
    for node in ast.walk(ast.parse(source)):
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute):
            if node.func.attr == 'se':
                args = node.args[1] if len(node.args) > 1 else None
                if not isinstance(args, ast.List) or '--json' not in literal_flags(args):
                    missing.append(node.lineno)
        if not isinstance(node, ast.List):
            continue
        direct = any(isinstance(item, ast.Call) and isinstance(item.func, ast.Name)
                     and item.func.id == 'str' and len(item.args) == 1
                     and isinstance(item.args[0], ast.Attribute)
                     and item.args[0].attr == 'se' for item in node.elts)
        generated = any(isinstance(item, ast.Starred)
                        and isinstance(item.value, ast.Attribute)
                        and isinstance(item.value.value, ast.Name)
                        and item.value.value.id == 'control'
                        and item.value.attr == 'entry' for item in node.elts)
        if direct or generated:
            flags = literal_flags(node)
            # These commands own a framed transport or fixed version string,
            # rather than an ordinary receipt whose default format can change.
            if flags and flags[0] in ('mcp', 'floor-serve', '--version'):
                continue
            if '--json' not in flags or ('--' in flags and flags.index('--json') > flags.index('--')):
                missing.append(node.lineno)
    return sorted(set(missing))


class JsonReceipts(unittest.TestCase):
    def test_all_python_stateengine_receipt_invocations_select_json(self):
        files = subprocess.check_output(['git', 'ls-files', '*.py'], cwd=ROOT, text=True).splitlines()
        failures = []
        for name in files:
            if Path(name).name.startswith('test_'):
                continue
            source = (ROOT / name).read_text(encoding='utf-8')
            failures.extend(f'{name}:{line}' for line in missing_json(source))
        self.assertFalse(failures, 'parsed se CLI invocations need --json: ' + ', '.join(failures))

    def test_guard_refuses_missing_json_and_json_passed_to_the_child(self):
        for source in [
            "control.se('init', ['init'])",
            "subprocess.run([str(self.se), *args])",
            "argv = [*control.entry, 'job', 'start', '--', python, job]",
            "argv = [*control.entry, 'job', 'start', '--', python, '--json']",
        ]:
            with self.subTest(source=source):
                self.assertTrue(missing_json(source))
        for source in [
            "control.se('init', ['init', '--json'])",
            "subprocess.run([str(self.se), *args, '--json'])",
            "argv = [*control.entry, 'job', 'start', '--json', '--', python, job]",
            "argv = [str(self.se), 'mcp', '--project', project]",
            "argv = [str(self.se), '--version']",
        ]:
            self.assertEqual(missing_json(source), [])

    def test_bundled_cli_parses_a_producer_that_defaults_to_plain_text(self):
        fixture = object.__new__(Fixture)
        fixture.se = Path('se')
        fixture.environment = {}
        fixture.profile = {'project': 'private', 'repo_root': 'private', 'registry_root': 'private'}

        def producer(argv, **_options):
            output = b'{"packet_id":"fixture"}' if '--json' in argv else b'Packet ready.\n'
            return subprocess.CompletedProcess(argv, 0, output, b'')

        with patch('bundled_mcp_fixture.subprocess.run', side_effect=producer):
            self.assertEqual(fixture.cli(['packet']), {'packet_id': 'fixture'})

    def test_shell_sender_selects_json_for_its_parsed_cli_result(self):
        source = (ROOT / 'scripts/stateengine-native/shell_sender_fixture.mjs').read_text()
        calls = re.findall(r'spawnSync\([^;]+?bin/se\.mjs[^;]+?\);', source, re.S)
        self.assertTrue(calls, 'shell sender CLI invocation was not inspected')
        for call in calls:
            self.assertRegex(call, r"['\"]--json['\"]")

    def test_nightly_registry_parses_a_producer_that_defaults_to_plain_text(self):
        # Load the existing helper without executing a nightly or sending mail.
        path = ROOT / 'scripts/build/nightly_revert.py'
        spec = importlib.util.spec_from_file_location('p13_nightly_revert', path)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        original_path = sys.path[:]
        try:
            sys.path.insert(0, str(path.parent))
            spec.loader.exec_module(module)

            def producer(command):
                if '--json' not in command:
                    return 'One registered seat.\n'
                return json.dumps({'project_id': 'secode', 'source': 'stateengine-registry',
                                   'agents': [{'agent_id': 'fixture'}], 'lead_agent_id': 'fixture'})

            with patch.object(module, 'remote', side_effect=producer):
                self.assertEqual(module.registry()[1], 'fixture')
        finally:
            sys.path[:] = original_path
            sys.modules.pop(spec.name, None)

    def test_new_direct_rust_se_invocations_require_explicit_output_selection(self):
        files = subprocess.check_output(['git', 'ls-files', 'crates/*/src/*.rs'],
                                        cwd=ROOT, text=True).splitlines()
        for name in files:
            source = (ROOT / name).read_text(encoding='utf-8')
            for call in re.findall(r'Command\s*::\s*new\(\s*"se(?:\.exe)?"\s*\)[^;]+;', source):
                self.assertRegex(call, r'"(?:--json|--version|mcp|floor-serve)"',
                                 f'{name}: audit the new StateEngine CLI output consumer')


if __name__ == '__main__':
    unittest.main()
