"""Keep durable identity producers and consumers on the same spelling."""
from collections import Counter
import contextlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "rename_labels", Path(__file__).resolve().parents[1] / "rename/grapt.py")
rename = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(rename)
OLD = "se" + "code"


class IdentityLabelTests(unittest.TestCase):
    def test_escaped_labels_keep_their_identity_in_source_and_mutations(self):
        table = rename.rules([f"{OLD}-service", f"{OLD}-artifact"], [])
        for suffix in ["service/registration/1", "artifact/keyed/1", "event-envelope/1"]:
            for ending in ['"', r'\n"', r'\"', r'\\n"']:
                text = f'"{OLD}-{suffix}{ending}'
                with self.subTest(text=text):
                    self.assertEqual(rename.rename_text(text, table, Counter()), text)

    def test_dotted_user_agent_versions_and_artifact_names_still_rename(self):
        for suffix in ["test/0.1", "service"]:
            self.assertEqual(rename.rename_text(f'"{OLD}-{suffix}"',
                rename.rules([f"{OLD}-service"], []), Counter()), f'"grapt-{suffix}"')

    def test_dry_run_refuses_an_already_renamed_identity(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "scripts/build").mkdir(parents=True)
            producer = root / "producer.rs"
            producer.write_text('"' + "grapt-" + r'service/registration/1\n"')
            metadata = type("Metadata", (), {"stdout": '{"packages": []}'})()
            with contextlib.ExitStack() as stack:
                for name, value in [("ROOT", root), ("tracked", lambda: ["producer.rs"]),
                                    ("workspace_names", lambda: ([], [])),
                                    ("box_table", lambda **_: set())]:
                    stack.enter_context(patch.object(rename, name, value))
                stack.enter_context(patch.object(rename.subprocess, "run", return_value=metadata))
                stack.enter_context(patch("sys.argv", ["grapt.py", "--dry-run"]))
                stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                errors = stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                self.assertNotEqual(rename.main(), 0)
                self.assertIn("service/registration/1", errors.getvalue())

    def test_schema_contracts_include_excluded_producers_but_not_receipts(self):
        for path, old_version, new_version, expected in [
            ("packages/stateengine/payload-pins.json", 1, 1, 2),
            ("producer.py", 1, 1, 2),
            ("docs/receipts/2026-01-01.json", 1, 1, 0),
            ("packages/stateengine/payload-pins.json", 1, 2, 0),
        ]:
            with self.subTest(path=path, versions=(old_version, new_version)), \
                    tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / "scripts/build").mkdir(parents=True)
                producer = root / path
                producer.parent.mkdir(parents=True, exist_ok=True)
                producer.write_text(f'"{OLD}.payload.v{old_version}" # grapt-rename: keep')
                (root / "consumer.py").write_text(f'"grapt.payload.v{new_version}"')
                metadata = type("Metadata", (), {"stdout": '{"packages": []}'})()
                with contextlib.ExitStack() as stack:
                    for name, value in [("ROOT", root),
                            ("tracked", lambda: [path, "consumer.py"]),
                            ("workspace_names", lambda: ([], [])),
                            ("box_table", lambda **_: set())]:
                        stack.enter_context(patch.object(rename, name, value))
                    stack.enter_context(patch.object(rename.subprocess, "run", return_value=metadata))
                    stack.enter_context(patch("sys.argv", ["grapt.py", "--dry-run"]))
                    stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
                    errors = stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
                    self.assertEqual(rename.main(), expected, errors.getvalue())
                    if expected:
                        self.assertIn("split schema ids: payload.v1", errors.getvalue())
