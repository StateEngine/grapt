"""Release notices retain actual copyright and cover the complete resolved graph."""
import importlib.util
from pathlib import Path, PureWindowsPath
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    'release_licenses', Path(__file__).resolve().parents[1] / 'release/licenses.py')
licenses = importlib.util.module_from_spec(spec)
spec.loader.exec_module(licenses)


def document(text='Copyright (c) Author\nPermission is hereby granted'):
    crate = {'id': 'registry+example#one@1.0.0', 'name': 'one', 'version': '1.0.0',
             'source': 'registry+example'}
    return {'licenses': [{'id': 'MIT', 'text': text, 'used_by': [{'crate': crate}]}],
            'crates': [{'package': crate}]}


class LicenseTests(unittest.TestCase):
    def test_every_ordinary_gate_runs_license_policy(self):
        import gate_plan
        root = Path('/repo')
        for mode in ['affected', 'full']:
            self.assertIn(['python-fixture', 'scripts/release/licenses.py', 'check'],
                          gate_plan.commands(mode, [], set(), root, python='python-fixture'))

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / 'Cargo.lock').write_text(
            'version=3\n[[package]]\nname="app"\nversion="0.1.0"\n'
            'dependencies=["one", "two"]\n'
            '[[package]]\nname="one"\nversion="1.0.0"\n'
            'source="registry+https://example.com/index"\nchecksum="aaa"\n'
            '[[package]]\nname="two"\nversion="1.0.0"\n'
            'source="registry+https://example.com/index"\nchecksum="bbb"\n')
        (self.root / 'Cargo.toml').write_text('[workspace]\nmembers=["crates/app"]\n')
        (self.root / 'about.toml').write_text('ignore-dev-dependencies=true\n')
        (self.root / 'deny.toml').write_text('')
        self.manifest = self.root / 'crates/app/Cargo.toml'
        self.manifest.parent.mkdir(parents=True)
        self.manifest.write_text(
            '[package]\nname="app"\nversion="0.1.0"\n'
            '[dependencies]\none="1.0.0"\n[dev-dependencies]\ntwo="1.0.0"\n')
        configuration = self.root / 'scripts/release'
        configuration.mkdir(parents=True)
        (configuration / 'license-overrides.toml').write_text('package = []\n')

    def test_html_keeps_full_notice_and_escapes_markup(self):
        text = 'Copyright <Author>\nPermission is hereby granted'
        output = licenses.render(document(text), self.root)
        self.assertIn('Copyright &lt;Author&gt;', output)
        self.assertIn('Permission is hereby granted', output)
        self.assertIn('one 1.0.0', output)

    def test_generic_copyright_refuses_until_exact_version_has_published_text(self):
        fixture = document('Copyright (c) <year> <copyright holders>')
        with self.assertRaisesRegex(ValueError, 'Missing published copyright'):
            licenses.render(fixture, self.root)
        copyright = 'Copyright Author François\nPermission is hereby granted'
        (self.root / 'LICENSE-MIT').write_bytes(copyright.encode('utf-8'))
        (self.root / 'scripts/release/license-overrides.toml').write_text(
            '[[package]]\nname="one"\nversion="1.0.0"\nlicense="MIT"\n'
            'text="LICENSE-MIT"\nsource="https://example.com/published/LICENSE"\n')
        self.assertIn('Copyright Author François', licenses.render(fixture, self.root))
        fixture['licenses'][0]['used_by'][0]['crate']['version'] = '2.0'
        with self.assertRaisesRegex(ValueError, 'Missing published copyright'):
            licenses.render(fixture, self.root)

    def test_a_crate_missing_its_license_is_not_silent(self):
        fixture = document()
        fixture['licenses'] = []
        with self.assertRaisesRegex(ValueError, 'does not cover'):
            licenses.render(fixture, self.root)

    def test_vendored_crates_keep_their_notices(self):
        fixture = document()
        crate = fixture['crates'][0]['package']
        crate['source'] = None
        crate['manifest_path'] = str(self.root / 'third-party/one/Cargo.toml')
        self.assertIn('Copyright (c) Author', licenses.render(fixture, self.root))

    def test_changed_lockfile_refuses_existing_register(self):
        notice = self.root / licenses.NOTICE
        notice.parent.mkdir(parents=True)
        notice.write_text(licenses.render(document(), self.root))
        licenses.verify(self.root)
        lock = self.root / 'Cargo.lock'
        lock.write_text(lock.read_text().replace('checksum="aaa"', 'checksum="changed"'))
        with self.assertRaisesRegex(ValueError, 'stale'):
            licenses.verify(self.root)

    def test_promoting_a_dev_dependency_refuses_the_old_register(self):
        notice = self.root / licenses.NOTICE
        notice.parent.mkdir(parents=True)
        notice.write_text(licenses.render(document(), self.root))
        self.manifest.write_text(self.manifest.read_text().replace('[dev-dependencies]\n', ''))
        with self.assertRaisesRegex(ValueError, 'stale'):
            licenses.verify(self.root)

    def test_windows_line_endings_preserve_the_dependency_fingerprint(self):
        manifest = self.root / 'Cargo.toml'
        contents = manifest.read_bytes()
        notice = self.root / licenses.NOTICE
        notice.parent.mkdir(parents=True)
        notice.write_text(licenses.render(document(), self.root))
        manifest.write_bytes(contents.replace(b'\n', b'\r\n'))
        licenses.verify(self.root)

    def test_windows_path_order_preserves_the_dependency_fingerprint(self):
        class WindowsOrderedPath(type(self.root)):
            def __lt__(self, other):
                return PureWindowsPath(self.as_posix()) < PureWindowsPath(other.as_posix())

        # These names sort differently with Windows case folding.
        configuration = self.root / 'scripts/release/license-overrides.toml'
        configuration.write_text(
            '[[package]]\nname="upper"\nversion="1.0.0"\ntext="Z-license"\n'
            '[[package]]\nname="lower"\nversion="1.0.0"\ntext="a-license"\n')
        for name in ['Z-license', 'a-license']:
            (self.root / name).write_text(name)
        digest = licenses.dependency_digest(self.root)
        with patch.object(licenses, 'Path', WindowsOrderedPath):
            self.assertEqual(licenses.dependency_digest(self.root), digest)

    def test_internal_metadata_and_dev_checksum_preserve_the_register(self):
        notice = self.root / licenses.NOTICE
        notice.parent.mkdir(parents=True)
        notice.write_text(licenses.render(document(), self.root))
        self.manifest.write_text(self.manifest.read_text().replace('version="0.1.0"', 'version="0.2.0"'))
        lock = self.root / 'Cargo.lock'
        lock.write_text(lock.read_text().replace('version="0.1.0"', 'version="0.2.0"')
                        .replace('checksum="bbb"', 'checksum="changed"'))
        licenses.verify(self.root)

    def test_internal_workspace_crates_and_edges_do_not_change_the_digest(self):
        notice = self.root / licenses.NOTICE
        notice.parent.mkdir(parents=True)
        notice.write_text(licenses.render(document(), self.root))
        licenses.verify(self.root)
        helper = self.root / 'crates/helper/Cargo.toml'
        helper.parent.mkdir(parents=True)
        helper.write_text('[package]\nname="helper"\nversion="0.1.0"\n')
        (self.root / 'Cargo.toml').write_text('[workspace]\nmembers=["crates/app", "crates/helper"]\n')
        self.manifest.write_text(
            '[package]\nname="app"\nversion="0.1.0"\n'
            '[dependencies]\none="1.0.0"\nhelper={path="../helper"}\n'
            '[dev-dependencies]\ntwo="1.0.0"\n')
        lock = self.root / 'Cargo.lock'
        lock.write_text(
            lock.read_text().replace('dependencies=["one", "two"]', 'dependencies=["helper", "one", "two"]')
            + '[[package]]\nname="helper"\nversion="0.1.0"\n')
        licenses.verify(self.root)

    def test_changed_vendored_license_refuses_the_register(self):
        vendor = self.root / 'third-party/one'
        vendor.mkdir(parents=True)
        (vendor / 'Cargo.toml').write_text('[package]\nname="one"\nversion="1.0.0"\nlicense="MIT"\n')
        license_file = vendor / 'LICENSE'
        license_file.write_text('Copyright Author\nPermission is hereby granted')
        self.manifest.write_text(
            '[package]\nname="app"\nversion="0.1.0"\n'
            '[dependencies]\none={path="../../third-party/one"}\n'
            '[dev-dependencies]\ntwo="1.0.0"\n')
        lock = self.root / 'Cargo.lock'
        lock.write_text(lock.read_text().replace(
            'source="registry+https://example.com/index"\nchecksum="aaa"\n', ''))
        notice = self.root / licenses.NOTICE
        notice.parent.mkdir(parents=True)
        fixture = document()
        crate = fixture['crates'][0]['package']
        crate.update(source=None, manifest_path=str(vendor / 'Cargo.toml'))
        notice.write_text(licenses.render(fixture, self.root))
        license_file.write_text('Copyright Another Author\nPermission is hereby granted')
        with self.assertRaisesRegex(ValueError, 'stale'):
            licenses.verify(self.root)

    def test_enabling_a_vendored_feature_refuses_the_old_register(self):
        vendor = self.root / 'third-party/one'
        vendor.mkdir(parents=True)
        manifest = vendor / 'Cargo.toml'
        manifest.write_text(
            '[package]\nname="one"\nversion="1.0.0"\nlicense="MIT"\n'
            '[dependencies]\ntwo={version="1.0.0",optional=true}\n'
            '[features]\ndefault=[]\n')
        self.manifest.write_text(
            '[package]\nname="app"\nversion="0.1.0"\n'
            '[dependencies]\none={path="../../third-party/one"}\n'
            '[dev-dependencies]\ntwo="1.0.0"\n')
        lock = self.root / 'Cargo.lock'
        lock.write_text(lock.read_text().replace(
            'source="registry+https://example.com/index"\nchecksum="aaa"\n',
            'dependencies=["two"]\n'))
        fixture = document()
        fixture['crates'][0]['package'].update(
            source=None, manifest_path=str(manifest))
        notice = self.root / licenses.NOTICE
        notice.parent.mkdir(parents=True)
        notice.write_text(licenses.render(fixture, self.root))
        licenses.verify(self.root)
        manifest.write_text(manifest.read_text().replace(
            'default=[]', 'default=["dep:two"]'))
        with self.assertRaisesRegex(ValueError, 'stale'):
            licenses.verify(self.root)

    def test_swapping_normal_and_dev_versions_of_same_vendored_crate_changes_digest(self):
        v1 = self.root / 'third-party/vend-1'
        v2 = self.root / 'third-party/vend-2'
        v1.mkdir(parents=True)
        v2.mkdir(parents=True)
        (v1 / 'Cargo.toml').write_text('[package]\nname="vend"\nversion="1.0.0"\nlicense="MIT"\n')
        (v2 / 'Cargo.toml').write_text('[package]\nname="vend"\nversion="2.0.0"\nlicense="Apache-2.0"\n')
        (self.root / 'Cargo.lock').write_text(
            'version=3\n[[package]]\nname="app"\nversion="0.1.0"\n'
            'dependencies=["vend 1.0.0", "vend 2.0.0"]\n'
            '[[package]]\nname="vend"\nversion="1.0.0"\n'
            '[[package]]\nname="vend"\nversion="2.0.0"\n')
        self.manifest.write_text(
            '[package]\nname="app"\nversion="0.1.0"\n'
            '[dependencies]\nvend={path="../../third-party/vend-1"}\n'
            '[dev-dependencies]\nvend_dev={package="vend",path="../../third-party/vend-2"}\n')
        before = licenses.dependency_digest(self.root)
        # Swap normal vs dev-only version while keeping Cargo.lock byte-identical.
        self.manifest.write_text(
            '[package]\nname="app"\nversion="0.1.0"\n'
            '[dependencies]\nvend={path="../../third-party/vend-2"}\n'
            '[dev-dependencies]\nvend_dev={package="vend",path="../../third-party/vend-1"}\n')
        after = licenses.dependency_digest(self.root)
        self.assertNotEqual(before, after)

    def test_toggling_vendored_dependency_features_with_identical_lockfile_changes_digest(self):
        v1 = self.root / 'third-party/vend-1'
        v1.mkdir(parents=True)
        (v1 / 'Cargo.toml').write_text(
            '[package]\nname="vend"\nversion="1.0.0"\nlicense="MIT"\n'
            '[features]\ndefault=[]\nextra=[]\n')
        (self.root / 'Cargo.lock').write_text(
            'version=3\n[[package]]\nname="app"\nversion="0.1.0"\n'
            'dependencies=["vend 1.0.0"]\n'
            '[[package]]\nname="vend"\nversion="1.0.0"\n')
        self.manifest.write_text(
            '[package]\nname="app"\nversion="0.1.0"\n'
            '[dependencies]\nvend={path="../../third-party/vend-1"}\n')
        before = licenses.dependency_digest(self.root)
        self.manifest.write_text(
            '[package]\nname="app"\nversion="0.1.0"\n'
            '[dependencies]\nvend={path="../../third-party/vend-1",features=["extra"]}\n')
        after_feature = licenses.dependency_digest(self.root)
        self.assertNotEqual(before, after_feature)
        self.manifest.write_text(
            '[package]\nname="app"\nversion="0.1.0"\n'
            '[dependencies]\nvend={path="../../third-party/vend-1",default-features=false,features=["extra"]}\n')
        after_default_toggle = licenses.dependency_digest(self.root)
        self.assertNotEqual(after_feature, after_default_toggle)
