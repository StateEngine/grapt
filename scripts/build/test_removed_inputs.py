"""Deletion must preserve retained callers, joined paths and input manifests."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import removed_inputs

RETIRED_MODULE_BASELINE = 'fixtures/rust-reconstruction/rust-module-size-baseline.json'
RETIRED_MODULE_BASELINE_DIR_REFS = frozenset({
    'crates/grapt-product/src/compose/tests.rs',
    'crates/grapt-provider/tests/d2_transcripts.rs',
    'remote-compute/test_profile.py',
    'scripts/build/install-fixtures.sh',
    'scripts/build/publication_scan.py',
    'scripts/build/windows_native_environment.py',
    'test-support/fixtures.rs',
})


def assert_retired_module_baseline_allowlist(case, root, retained):
    """Allow only parent-directory fixture refs, never concrete baseline reads."""
    expected = set(RETIRED_MODULE_BASELINE_DIR_REFS) | {'scripts/build/test_removed_inputs.py'}
    case.assertEqual(retained.pop(RETIRED_MODULE_BASELINE, set()), expected)
    basename = Path(RETIRED_MODULE_BASELINE).name
    stem = Path(RETIRED_MODULE_BASELINE).stem
    for relative in sorted(RETIRED_MODULE_BASELINE_DIR_REFS):
        source = root / relative
        case.assertNotIn(
            RETIRED_MODULE_BASELINE,
            removed_inputs.references(source, root, set()),
            f'{relative} resolves a concrete reference to {RETIRED_MODULE_BASELINE}',
        )
        text = source.read_text(encoding='utf-8')
        case.assertNotIn(basename, text, f'{relative} directly references {basename}')
        case.assertNotIn(stem, text, f'{relative} directly references {stem}')


class RemovedInputTests(unittest.TestCase):
    def test_current_deletions_have_no_retained_consumers(self):
        root = Path(__file__).resolve().parents[2]
        removed = removed_inputs.deleted_paths(root, os.environ.get('SECODE_GATE_BASE'))
        if removed:
            retained = removed_inputs.consumers(root, removed)
            if 'THIRD_PARTY_NOTICES.md' in removed:
                # This retired source is now a generated package output. Keep
                # the exact writers/compatibility fixtures visible: any new
                # consumer, including a stale include_str!, still fails here.
                self.assertEqual(retained.pop('THIRD_PARTY_NOTICES.md', set()), {
                    'scripts/release/notices.py', 'scripts/release/package.py',
                    'scripts/release/test_package_refusals.py',
                    'scripts/build/landing.py', 'scripts/build/test_landing_documents.py',
                    'scripts/build/test_landing_licenses.py',
                    'scripts/build/test_removed_inputs.py',
                })
            # The product rename moved this test; a dated receipt's captured
            # panic log still quotes the old path, and recorded output stays as run.
            moved = 'crates/secode-product/tests/dogfood_headless/cli_compatibility.rs'  # grapt-rename: keep
            if moved in removed:
                self.assertEqual(retained.pop(moved, set()), {
                    'docs/cli-compatibility/receipts/windows/ccm-w5-ccm111-mutant-20260925T103445Z.json',
                    'scripts/build/test_removed_inputs.py',
                })
            # The rename's reviewed table records the exact pre-rename lines it applies to.
            table_path = 'crates/secode-release/mutations/r6mr/run.py'  # grapt-rename: keep
            if table_path in removed:
                self.assertEqual(retained.pop(table_path, set()), {
                    'scripts/rename/box-rename-table.json',
                    'scripts/build/test_removed_inputs.py',
                })
            # The retired module-size baseline was deleted; these files only
            # reference the parent fixture directory (or this exact allowlist).
            if RETIRED_MODULE_BASELINE in removed:
                assert_retired_module_baseline_allowlist(self, root, retained)
            self.assertEqual(retained, {})

    def test_large_data_keeps_tail_references_and_separate_source_and_data_bounds(self):
        import json
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = 'fixtures/catalog-tail.json'
            source = root / 'catalog.json'
            source.write_text(json.dumps({'padding': 'x' * removed_inputs.MAX_BYTES, 'input': target}))
            self.assertIn(target, removed_inputs.references(source, root, {target}))
            code = root / 'large.rs'
            code.write_text('x' * (removed_inputs.MAX_BYTES + 1))
            with self.assertRaisesRegex(ValueError, 'scan bound'):
                removed_inputs.references(code, root, {target})
            source.write_text('x' * (removed_inputs.MAX_DATA_BYTES + 1))
            with self.assertRaisesRegex(ValueError, 'scan bound'):
                removed_inputs.references(source, root, {target})

    def test_real_git_deletion_finds_joined_python_and_rust_inputs(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            subprocess.run(['git', 'config', 'user.name', 'Fixture'], cwd=root, check=True)
            subprocess.run(['git', 'config', 'user.email', 'fixture@example.invalid'], cwd=root, check=True)
            (root / 'fixtures').mkdir()
            (root / 'fixtures/input.json').write_text('{}')
            (root / 'reader.py').write_text("from pathlib import Path\nROOT=Path(__file__).parent\n"
                "CORPUS=ROOT/'fixtures'\nprint((CORPUS/'input.json').read_text())\n")
            (root / 'lib.rs').write_text('const INPUT: &str = include_str!("fixtures/input.json");')
            subprocess.run(['git', 'add', 'fixtures/input.json', 'reader.py', 'lib.rs'], cwd=root, check=True)
            subprocess.run(['git', 'commit', '-qm', 'fixture'], cwd=root, check=True)
            (root / 'fixtures/input.json').unlink()
            removed = removed_inputs.deleted_paths(root)
            self.assertEqual(removed, {'fixtures/input.json'})
            self.assertEqual(removed_inputs.consumers(root, removed),
                             {'fixtures/input.json': {'reader.py', 'lib.rs'}})
            self.assertEqual(removed_inputs.consumers(root, removed | {'reader.py', 'lib.rs'}), {})

    def test_consumed_manifest_keeps_its_inputs_and_rust_joined_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            (root / 'fixtures').mkdir()
            (root / 'fixtures/index.json').write_text('{"input":"fixtures/child.json"}')
            (root / 'reader.py').write_text("from pathlib import Path\n"
                "ROOT=Path(__file__).parent\nDATA=ROOT.joinpath('fixtures','index.json')\n"
                "print(DATA.read_text())\n")
            (root / 'lib.rs').write_text('let input = root.join("fixtures").join("other.json");\n'
                '// docs/contract.md.\n')
            subprocess.run(['git', 'add', 'reader.py', 'lib.rs', 'fixtures/index.json'], cwd=root, check=True)
            removed = {'fixtures/child.json', 'fixtures/other.json', 'docs/contract.md'}
            self.assertEqual(removed_inputs.consumers(root, removed), {
                'fixtures/child.json': {'fixtures/index.json'},
                'fixtures/other.json': {'lib.rs'}, 'docs/contract.md': {'lib.rs'}})

    def test_filename_argument_under_a_named_fixture_directory_is_retained(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'reader.py'
            source.write_text("from pathlib import Path\nHERE=Path(__file__).parent\n"
                "DATA=HERE/'fixtures/corpus'\n"
                "def read(name): return (DATA/name).read_bytes()\nread('value.json')\n")
            candidates = {'fixtures/corpus/value.json'}
            self.assertIn('fixtures/corpus/value.json', removed_inputs.references(source, root, candidates))

    def test_a_discovery_glob_relies_on_a_deleted_path_only_when_nothing_else_matches(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'runner.py'
            source.write_text("from pathlib import Path\nROOT=Path(__file__).parent\n"
                              "MANIFESTS=sorted(ROOT.glob('crates/*/Cargo.toml'))\n")
            gone = 'crates/old/Cargo.toml'
            (root / 'crates/new').mkdir(parents=True)
            (root / 'crates/new/Cargo.toml').write_text('[package]\n')
            self.assertNotIn(gone, removed_inputs.references(source, root, {gone}))
            (root / 'crates/new/Cargo.toml').unlink()
            self.assertIn(gone, removed_inputs.references(source, root, {gone}))

    def test_rust_filename_argument_under_a_literal_helper_directory_is_retained(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'reader.rs'
            source.write_text('fn script(name: &str) { root.join("scripts/qualification/recovery").join(name); }\n'
                              'script("permission_driver.py");\n')
            target = 'scripts/qualification/recovery/permission_driver.py'
            self.assertIn(target, removed_inputs.references(source, root, {target}))

    def test_manifest_declared_roots_keep_their_source_and_manifest_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'Cargo.toml'
            source.write_text('[workspace]\nmembers=[]\nexclude=["scripts/qualification/adapter"]\n')
            targets = {'scripts/qualification/adapter/Cargo.toml',
                       'scripts/qualification/adapter/src/main.rs',
                       'scripts/qualification/other/src/main.rs'}
            found = removed_inputs.references(source, root, targets)
            self.assertTrue((targets - {'scripts/qualification/other/src/main.rs'}) <= found)
            self.assertNotIn('scripts/qualification/other/src/main.rs', found)

    def test_retired_module_baseline_allowlist_rejects_concrete_reads(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for relative in RETIRED_MODULE_BASELINE_DIR_REFS:
                target = root / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text('fixtures/rust-reconstruction\n', encoding='utf-8')
            expected = set(RETIRED_MODULE_BASELINE_DIR_REFS) | {
                'scripts/build/test_removed_inputs.py',
            }
            assert_retired_module_baseline_allowlist(
                self, root, {RETIRED_MODULE_BASELINE: set(expected)}
            )
            fixtures_rs = root / 'test-support/fixtures.rs'
            for concrete in (
                'const _: &str = include_str!("fixtures/rust-reconstruction/rust-module-size-baseline.json");\n',
                'let _ = root.join("fixtures/rust-reconstruction").join("rust-module-size-baseline.json");\n',
                'const STEM: &str = "rust-module-size-baseline";\n',
            ):
                fixtures_rs.write_text(concrete, encoding='utf-8')
                with self.assertRaises(AssertionError):
                    assert_retired_module_baseline_allowlist(
                        self, root, {RETIRED_MODULE_BASELINE: set(expected)}
                    )
            fixtures_rs.write_text('fixtures/rust-reconstruction\n', encoding='utf-8')
            py_file = root / 'remote-compute/test_profile.py'
            py_file.write_text(
                "from pathlib import Path\n"
                "ROOT = Path(__file__).resolve().parents[1]\n"
                "PREFIX = 'rust-module-size-'\n"
                "TARGET = ROOT / 'fixtures/rust-reconstruction' / (PREFIX + 'baseline.json')\n",
                encoding='utf-8',
            )
            with self.assertRaises(AssertionError):
                assert_retired_module_baseline_allowlist(
                    self, root, {RETIRED_MODULE_BASELINE: set(expected)}
                )
