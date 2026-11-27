"""Branding checks include their own source and narrow legal exceptions."""
from pathlib import Path
import subprocess
import tempfile
import unittest

import branding


class BrandingTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def write(self, name, content):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)
        return name

    def test_mixed_case_content_and_filename_are_both_rejected(self):
        legacy = 'OpEn' + 'CoDe'
        name = self.write(f'crates/{legacy}/asset.txt', f'Use {legacy} today')
        self.assertEqual(len(branding.violations(self.root, [name])), 2)

    def test_only_the_attribution_register_can_name_the_upstream(self):
        notice = self.write(branding.ATTRIBUTION_ROOT + 'x.md',
                            f'Copyright 2025 {branding.LEGACY}; MIT License')
        self.assertEqual(branding.violations(self.root, [notice]), [])
        for name in ['crates/other.rs', 'crates/grapt-provider/src/effort.rs',
                     'docs/daily-driver/guide.md']:
            with self.subTest(name=name):
                self.write(name, f'Copyright 2025 {branding.LEGACY}; MIT License')
                self.assertEqual(len(branding.violations(self.root, [name])), 1)

    def test_legacy_config_literal_is_exact_and_confined_to_its_boundary(self):
        declaration = branding.LEGACY_CONFIG_DECLARATION.decode()
        boundary = self.write(branding.COMPATIBILITY_FILE, declaration)
        self.assertEqual(branding.violations(self.root, [boundary]), [])
        for name in ['crates/grapt-cli/src/seat.rs', 'docs/daily-driver/guide.md']:
            with self.subTest(name=name):
                self.write(name, declaration)
                self.assertEqual(len(branding.violations(self.root, [name])), 1)
        for content in [declaration + ' // ' + branding.LEGACY,
                        declaration.replace('_CONFIG"', '_OTHER"'),
                        declaration + '\nLaunch ' + branding.LEGACY]:
            with self.subTest(content=content):
                self.write(boundary, content)
                self.assertEqual(len(branding.violations(self.root, [boundary])), 1)

    def test_legal_content_never_exempts_a_filename(self):
        name = self.write(f'crates/{branding.LEGACY}.rs', branding.LEGACY)
        self.assertEqual(len(branding.violations(self.root, [name])), 2)

    def test_benchmark_filenames_are_checked_without_rebranding_competitors(self):
        ordinary = self.write('bench/native-model-limits.json', branding.LEGACY)
        self.assertEqual(branding.violations(self.root, [ordinary]), [])
        inherited = self.write(f'bench/{branding.LEGACY.upper()}-native-limits.JSON', '{}')
        self.assertEqual(len(branding.violations(self.root, [inherited])), 1)

    def test_guard_and_its_tests_have_no_self_exemption(self):
        for name in ['scripts/build/branding.py', 'scripts/build/test_branding.py']:
            self.assertIn(name, branding.SURFACES)
            self.assertFalse(name.startswith(branding.ATTRIBUTION_ROOT))
            self.write(name, branding.LEGACY)
            self.assertEqual(len(branding.violations(self.root, [name])), 1)

    def test_binary_assets_are_scanned_without_a_text_encoding_assumption(self):
        name = self.write('crates/assets/image.bin', '')
        (self.root / name).write_bytes(b'\xff\0' + branding.LEGACY.encode())
        self.assertEqual(len(branding.violations(self.root, [name])), 1)

    def test_git_scan_includes_new_files_and_packaging_surfaces(self):
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        for surface in ['crates', 'scripts/release', 'scripts/package',
                        'packages/plugin-bridge', 'docs/release-packaging',
                        'docs/daily-driver']:
            self.write(f'{surface}/new.txt', branding.LEGACY)
        self.assertEqual(len(branding.check(self.root)), 6)
        self.write(f'bench/{branding.LEGACY}-native-limits.json', '{}')
        self.assertEqual(len(branding.check(self.root)), 7)

    def test_public_catalog_preserves_external_names_without_exempting_source(self):
        import json
        provider = branding.LEGACY
        self.write(branding.CATALOG, json.dumps({provider: {
            'id': provider, 'name': provider, 'doc': 'https://example.invalid/' + provider,
            'models': {}, 'padding': 'x' * branding.MAX_FILE_BYTES}}))
        self.assertEqual(branding.violations(self.root, [branding.CATALOG]), [])
        source = self.write('crates/provider/banner.rs', provider)
        self.assertEqual(len(branding.violations(self.root, [source])), 1)
        self.write(branding.CATALOG, json.dumps({'message': 'Launch ' + provider}))
        with self.assertRaisesRegex(ValueError, 'metadata shape'):
            branding.violations(self.root, [branding.CATALOG])
        self.write(branding.CATALOG, 'x' * (branding.MAX_CATALOG_BYTES + 1))
        with self.assertRaisesRegex(ValueError, 'byte bound'):
            branding.violations(self.root, [branding.CATALOG])

    def test_current_shipped_tree_passes(self):
        self.assertEqual(branding.check(Path(__file__).resolve().parents[2]), [])

    def competitor_references(self):
        competitor = branding.LEGACY + '2'
        return [
            f'qualified value among Codex 0.164.0, Claude Code 2.1.310, {competitor} 2.1.4 and the',
            f'{competitor} and TS frontend CPU is unavailable because separate parent/nested-job',
        ]

    def test_fixed_performance_comparisons_keep_the_actual_competitor_name(self):
        name = self.write(branding.CHECKLIST, '\n'.join(self.competitor_references()))
        self.assertEqual(branding.violations(self.root, [name]), [])

    def test_comparison_exception_does_not_exempt_other_checklist_branding(self):
        for content in [
            'Use ' + branding.LEGACY + ' as this product',
            '## Fixed native TUI performance requirements\nLaunch ' + branding.LEGACY + '2 today',
            *[reference + ' and launch ' + branding.LEGACY for reference in self.competitor_references()],
            *[reference.replace(branding.LEGACY + '2', branding.LEGACY) for reference in self.competitor_references()],
        ]:
            with self.subTest(content=content):
                name = self.write(branding.CHECKLIST, content)
                self.assertEqual(len(branding.violations(self.root, [name])), 1)

    def test_competitor_references_do_not_exempt_shipped_surfaces_or_filenames(self):
        for name in ['README.md', 'crates/brand.rs', 'docs/daily-driver/guide.md',
                     'scripts/build/branding.py', 'scripts/build/test_branding.py']:
            with self.subTest(name=name):
                self.write(name, '\n'.join(self.competitor_references()))
                self.assertEqual(len(branding.violations(self.root, [name])), 2)
        name = self.write('docs/' + branding.LEGACY + '2.md', self.competitor_references()[0])
        self.assertEqual(len(branding.violations(self.root, [name])), 2)
