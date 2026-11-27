"""Compose real Git license conflicts through the release generator and gate."""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

import landing
from test_landing import command, commit, fixture, invoke, receipt_input, exported_checkout

REGISTER = landing.GENERATED_REGISTER
RELEASE = Path(landing.__file__).resolve().parents[1] / 'release'


@contextmanager
def license_fixture():
    with fixture() as (outer, canonical, root, _, env):
        release = canonical / 'scripts/release'
        release.mkdir()
        for name in ('licenses.py', 'notices.py'):
            shutil.copyfile(RELEASE / name, release / name)
        (canonical / 'notices').mkdir()
        (canonical / 'notices/a.md').write_text('fixture\n')
        (release / 'license-overrides.toml').write_text('package=[]\n')
        (canonical / 'about.toml').write_text('accepted=["MIT"]\n')
        (canonical / 'deny.toml').write_text('[licenses]\nallow=["MIT"]\n')
        crates = [{'id': name, 'name': name, 'version': '0.1.0', 'source': None,
                   'manifest_path': str(canonical / name / 'Cargo.toml')} for name in ('a', 'b')]
        document = {'licenses': [{'id': 'MIT', 'text': 'Published fixture license',
                                 'used_by': [{'crate': crate} for crate in crates]}],
                    'crates': [{'package': crate} for crate in crates]}
        tools = outer / 'tools'
        tools.mkdir()
        metadata = tools / 'about.json'
        metadata.write_text(json.dumps(document))
        for name, version in [('cargo-deny', '0.20.2'), ('cargo-about', '0.9.2')]:
            tool = tools / name
            tool.write_text(f'#!{sys.executable}\nimport pathlib,sys\n'
                            f'if sys.argv[1:]==["--version"]: print("{name} {version}")\n'
                            'elif "--output-file" in sys.argv:\n'
                            ' pathlib.Path(sys.argv[sys.argv.index("--output-file")+1]).write_bytes('
                            f'pathlib.Path({str(metadata)!r}).read_bytes())\n')
            tool.chmod(0o700)
        env['PATH'] = str(tools) + ':' + env['PATH']
        # Stub fetches must never stamp the real advisory cache as fresh.
        env['CARGO_HOME'] = str(outer / 'cargo-home')
        command(canonical, sys.executable, 'scripts/release/licenses.py', 'generate', env=env)
        base = commit(canonical, ['scripts/release/licenses.py', 'scripts/release/notices.py', 'notices/a.md',
                                  'scripts/release/license-overrides.toml',
                                  'about.toml', 'deny.toml', REGISTER])
        command(root, 'git', 'fetch', '-q', str(canonical), 'main')
        command(root, 'git', 'reset', '--hard', 'FETCH_HEAD')
        yield outer, canonical, root, base, env


def advance(root, env, input_path):
    with (root / input_path).open('a') as output:
        output.write('# independent dependency policy edit\n')
    command(root, sys.executable, 'scripts/release/licenses.py', 'generate', env=env)
    return commit(root, [input_path, REGISTER])


class LicenseCompositionTests(unittest.TestCase):
    def test_generated_register_and_human_notice_have_separate_overlap_rules(self):
        self.assertTrue(landing.assert_disjoint({REGISTER}, {REGISTER}))
        self.assertFalse(landing.assert_disjoint({'THIRD_PARTY_NOTICES.md'}, {'THIRD_PARTY_NOTICES.md'}))
        for owned, advanced in [({'THIRD_PARTY_NOTICES.MD'}, {'THIRD_PARTY_NOTICES.md'}),
                                ({'licenses/published.txt'}, {'licenses/published.txt'}),
                                ({REGISTER}, {'licenses'}),
                                ({REGISTER.upper()}, {REGISTER}),
                                ({'scripts/release/license-overrides.toml'},
                                 {'scripts/release/license-overrides.toml'})]:
            with self.subTest(owned=owned), self.assertRaisesRegex(ValueError, 'overlaps'):
                landing.assert_disjoint(owned, advanced)

    def test_notice_conflict_regenerates_checks_and_lands_the_composed_tree(self):
        with license_fixture() as (outer, canonical, root, base, env):
            with (root / 'about.toml').open('a') as output:
                output.write('# candidate dependency policy\n')
            commit(root, ['about.toml'])
            command(root, sys.executable, 'scripts/release/licenses.py', 'generate', env=env)
            accepted = commit(root, [REGISTER])
            advanced = advance(canonical, env, 'scripts/release/license-overrides.toml')
            request = receipt_input(outer, root, base, accepted)
            original = Path(request['gate_log']).with_suffix('.log.receipt.json').read_bytes()
            result = invoke(root, request, env, timeout=180)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            record = json.loads(Path(request['receipt']).read_text())
            self.assertEqual(record['generated_register']['path'], REGISTER)
            self.assertTrue(record['commands'], 'composition checks were omitted')
            self.assertEqual(original, Path(request['gate_log']).with_suffix('.log.receipt.json').read_bytes())
            request.update(candidate=record['candidate'], new_base=advanced)
            verified = invoke(root, request, env, 'verify')
            self.assertEqual(verified.returncode, 0, verified.stdout + verified.stderr)
            with exported_checkout(root, request):
                fresh = (root / REGISTER).read_bytes()
                command(root, sys.executable, 'scripts/release/licenses.py', 'generate', env=env)
                self.assertEqual((root / REGISTER).read_bytes(), fresh)
                self.assertEqual(command(root, 'git', 'status', '--porcelain'), '')
            command(canonical, 'git', 'fetch', '-q', str(root), landing.export_branch(request))
            command(canonical, 'git', 'merge', '--ff-only', 'FETCH_HEAD')
            self.assertEqual(command(canonical, 'git', 'rev-parse', 'HEAD'), record['candidate'])
            self.assertEqual((canonical / REGISTER).read_bytes(), fresh)

    def test_source_overlap_still_refuses_before_rebase(self):
        with license_fixture() as (outer, canonical, root, base, env):
            accepted = advance(root, env, 'about.toml')
            advance(canonical, env, 'about.toml')
            # Different comments make the source edits distinct commits.
            with (canonical / 'about.toml').open('a') as output:
                output.write('# canonical source conflict\n')
            commit(canonical, ['about.toml'])
            request = receipt_input(outer, root, base, accepted)
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('overlaps candidate path: about.toml', result.stderr)
            self.assertEqual(command(root, 'git', 'rev-parse', 'HEAD'), accepted)
            self.assertFalse(Path(request['receipt']).exists())

    def test_corrupt_register_fails_check_and_derived_body_cannot_be_resealed(self):
        with license_fixture() as (outer, canonical, root, base, env):
            accepted = advance(root, env, 'about.toml')
            advanced = advance(canonical, env, 'scripts/release/license-overrides.toml')
            request = receipt_input(outer, root, base, accepted)
            result = invoke(root, request, env, timeout=180)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            path = Path(request['receipt'])
            record = json.loads(path.read_text())
            with exported_checkout(root, request):
                register = root / REGISTER
                original = register.read_text()
                register.write_text(original.replace('Dependency inputs SHA-256:', 'corrupted marker:'))
                with self.assertRaises(subprocess.CalledProcessError) as error:
                    command(root, sys.executable, 'scripts/release/licenses.py', 'check', env=env)
                self.assertIn('Dependency licenses are stale', str(error.exception.output))
                # A body change keeps the valid input marker; the separate proof
                # must bind all generated bytes, even if source identity is updated.
                register.write_text(original.replace('Published fixture license', 'tampered body'))
                candidate = commit(root, [REGISTER])
            request.update(candidate=candidate, new_base=advanced)
            record.update(candidate=candidate, source_before={'commit': candidate, 'clean': True},
                          source_after={'commit': candidate, 'clean': True})
            path.write_text(json.dumps(record))
            rejected = invoke(root, request, env, 'verify')
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn('Generated license register', rejected.stderr)

    def test_advisory_fetch_failure_uses_only_a_recent_cache(self):
        with license_fixture() as (outer, canonical, root, base, env):
            tools = outer / 'tools'
            fetches = tools / 'deny-fetches'
            checks = tools / 'deny-checks'
            stub = tools / 'cargo-deny'
            stub.write_text(
                f'#!{sys.executable}\nimport pathlib,sys\n'
                'if sys.argv[1:]==["--version"]: print("cargo-deny 0.20.2")\n'
                'elif "fetch" in sys.argv:\n'
                f' open({str(fetches)!r},"a").write("x"); sys.exit(1)\n'
                'elif "--output-file" in sys.argv:\n'
                ' pathlib.Path(sys.argv[sys.argv.index("--output-file")+1]).write_bytes('
                f'pathlib.Path({str(tools / "about.json")!r}).read_bytes())\n'
                f'else: open({str(checks)!r},"a").write(" ".join(sys.argv[1:])+"\\n")\n')
            stub.chmod(0o700)
            databases = Path(env['CARGO_HOME']) / 'advisory-dbs'
            # No cached database: refuse, named as a fetch failure, before any check.
            with self.assertRaises(subprocess.CalledProcessError) as error:
                command(root, sys.executable, 'scripts/release/licenses.py', 'check', env=env)
            self.assertIn('advisory fetch failure (network, not code)', str(error.exception.output))
            self.assertEqual(fetches.read_text(), 'xxx', 'fetch was not retried three times')
            self.assertFalse(checks.exists(), 'advisory check ran without a usable database')
            # A database fetched within 24 h is checked offline, with its age shown.
            (databases / 'advisory-db-fixture/.git').mkdir(parents=True)
            (databases / 'secode-advisory-fetch.stamp').write_text('fixture\n')
            output = command(root, sys.executable, 'scripts/release/licenses.py', 'check', env=env)
            self.assertIn('checking against the cached advisory database, last fetched 0.0 h ago', output)
            self.assertIn('--offline', checks.read_text())

    def test_advisory_cache_age_counts_only_a_successful_fetch_stamp(self):
        sys.path.insert(0, str(RELEASE))
        import licenses
        with tempfile.TemporaryDirectory() as temporary:
            home = Path(temporary)
            now = time.time()
            # A failed git fetch leaves a fresh, empty FETCH_HEAD: never fresh.
            fetch_head = home / 'advisory-dbs/advisory-db-fixture/.git/FETCH_HEAD'
            fetch_head.parent.mkdir(parents=True)
            fetch_head.write_text('')
            self.assertIsNone(licenses.advisory_cache_age(home, now))
            stamp = home / 'advisory-dbs' / licenses.ADVISORY_FETCH_STAMP
            stamp.write_text('fixture\n')
            for hours in (25, 1):
                os.utime(stamp, (now - hours * 3600, now - hours * 3600))
                age = licenses.advisory_cache_age(home, now)
                self.assertAlmostEqual(age, hours * 3600, delta=1)
            self.assertGreater(25 * 3600, licenses.ADVISORY_CACHE_MAX_AGE)
            shutil.rmtree(home / 'advisory-dbs/advisory-db-fixture')
            self.assertIsNone(licenses.advisory_cache_age(home, now), 'a stamp without a database counted')

    def test_successful_advisory_fetch_stamps_the_private_cache(self):
        with license_fixture() as (outer, canonical, root, base, env):
            command(root, sys.executable, 'scripts/release/licenses.py', 'check', env=env)
            stamp = Path(env['CARGO_HOME']) / 'advisory-dbs/secode-advisory-fetch.stamp'
            self.assertTrue(stamp.is_file(), 'a successful fetch left no freshness stamp')


    def test_refetch_blip_inside_the_check_reruns_offline_but_findings_still_fail(self):
        with license_fixture() as (outer, canonical, root, base, env):
            tools = outer / 'tools'
            checks = tools / 'deny-checks'
            stub = tools / 'cargo-deny'
            for finding in (False, True):
                stub.write_text(
                    f'#!{sys.executable}\nimport pathlib,sys\n'
                    'if sys.argv[1:]==["--version"]: print("cargo-deny 0.20.2")\n'
                    'elif "fetch" in sys.argv: pass\n'
                    'elif "--output-file" in sys.argv:\n'
                    ' pathlib.Path(sys.argv[sys.argv.index("--output-file")+1]).write_bytes('
                    f'pathlib.Path({str(tools / "about.json")!r}).read_bytes())\n'
                    'else:\n'
                    f' open({str(checks)!r},"a").write(" ".join(sys.argv[1:])+"\\n")\n'
                    + (' sys.stderr.write("error[vulnerability]: fixture advisory\\n"); sys.exit(1)\n' if finding else
                       ' if "--offline" not in sys.argv:\n'
                       '  sys.stderr.write("[ERROR] failed to fetch advisory database fixture\\n"); sys.exit(1)\n'))
                stub.chmod(0o700)
                checks.unlink(missing_ok=True)
                if finding:
                    with self.assertRaises(subprocess.CalledProcessError) as error:
                        command(root, sys.executable, 'scripts/release/licenses.py', 'check', env=env)
                    self.assertIn('fixture advisory', str(error.exception.output))
                    self.assertEqual(len(checks.read_text().splitlines()), 1, 'a finding was rerun offline')
                else:
                    output = command(root, sys.executable, 'scripts/release/licenses.py', 'check', env=env)
                    self.assertIn('rerunning the checks offline', output)
                    runs = checks.read_text().splitlines()
                    self.assertEqual(len(runs), 2)
                    self.assertNotIn('--offline', runs[0])
                    self.assertIn('--offline', runs[1])


if __name__ == '__main__':
    unittest.main()
