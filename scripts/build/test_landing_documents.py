"""Real Git composition of the single allow-listed human notice document."""
from contextlib import contextmanager
import json
import sys
from pathlib import Path
import unittest

import landing
from fixture_environment import isolated_environment
from test_landing import command, commit, fixture as compiler_fixture, invoke, receipt_input, exported_checkout

NOTICE = 'THIRD_PARTY_NOTICES.md'
BASE = 'First notice\n' + '\n'.join(f'Unchanged notice {i}' for i in range(20)) + '\nSecond notice\n'


@contextmanager
def fixture():
    # Execute the real affected/full planner, runner and verifier on a tiny
    # workspace. The law targets and licensing command are explicit fixture
    # stand-ins; their real rules run in this slice's repository affected gate.
    with compiler_fixture(a_source="pub fn value() -> u32 {\n    1\n}\n") as (outer, canonical, root, _, env):
        (canonical / 'Cargo.toml').write_text('[workspace]\nmembers=["a","b","grapt-types","grapt-store"]\nresolver="2"\n')
        for name, targets in [('grapt-types', ('no_god_files', 'direct_include_format', 'crate_graph')),
                              ('grapt-store', ('write_chokepoint',))]:
            crate = canonical / name
            (crate / 'tests').mkdir(parents=True)
            (crate / 'Cargo.toml').write_text(f'[package]\nname="{name}"\nversion="0.1.0"\nedition="2021"\n')
            for target in targets:
                (crate / 'tests' / (target + '.rs')).write_text('#[test]\nfn fixture_target() {}\n')
        release = canonical / 'scripts/release'
        release.mkdir()
        (release / 'licenses.py').write_text('import sys\nassert sys.argv[1:] == ["check"]\n')
        (canonical / 'scripts/build/test_features.py').write_text(
            'def arguments(packages=None): return []\ndef ordinary_commands(packages=None): return []\n')
        (canonical / 'scripts/build/test_fixture.py').write_text(
            'import unittest\nfrom pathlib import Path\nclass FixtureTest(unittest.TestCase):\n def test_notice(self): self.assertTrue(Path("THIRD_PARTY_NOTICES.md").is_file())\n')
        ratchet_files = {
            'crates/grapt-types/tests/include-ratchet.json': {'carriers': {}},
            'crates/grapt-types/tests/legacy-expect-ratchet.json': {'files': {}},
            'scripts/build/module-cycles.json': {'deep': [], 'configs': [], 'cycles': {}, 'edges': {}},
        }
        for rel, doc in ratchet_files.items():
            dest = canonical / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(json.dumps(doc) + '\n')
        gate = canonical / 'scripts/gate.sh'
        text = gate.read_text().replace('if [ "$1" != composition ]; then exit 0; fi\n', '')
        text = text.replace('import gate; ', 'import gate; gate.FULL_LOCK=Path.cwd().parent/"full.lock"; ')
        text = text.replace('gate.FULL_LOCK=Path.cwd().parent/"full.lock"; ',
                            'gate.FULL_LOCK=Path.cwd().parent/"full.lock"; gate.full_capacity.LOCK=gate.FULL_LOCK; ')
        text = text.replace('from pathlib import Path; ', '').replace('import signal, sys, tempfile;',
                          'import signal, sys, tempfile; from pathlib import Path;')
        gate.write_text(text)
        command(canonical, 'cargo', 'generate-lockfile', '--offline', env=env)
        tracked = ['Cargo.toml', 'Cargo.lock', 'scripts/gate.sh', 'scripts/build/test_features.py',
                   'scripts/release/licenses.py', 'scripts/build/test_fixture.py', 'a/src/lib.rs',
                   *ratchet_files.keys()]
        tracked += [p.relative_to(canonical).as_posix() for name in ('grapt-types', 'grapt-store')
                    for p in (canonical / name).rglob('*') if p.is_file()]
        base = commit(canonical, tracked)
        command(root, 'git', 'fetch', '-q', str(canonical), 'main')
        command(root, 'git', 'reset', '--hard', 'FETCH_HEAD')
        yield outer, canonical, root, base, env


def request_input(outer, root, base, accepted, full=False):
    request = receipt_input(outer, root, base, accepted, full=full)
    # Original receipts are synthetic test inputs. Match this fixture's feature
    # policy, rather than this test process's real workspace feature registry.
    path = Path(request['gate_log']).with_suffix('.log.receipt.json')
    record = json.loads(path.read_text())
    script = ('import json,sys,subprocess; from pathlib import Path; sys.path.insert(0,"scripts/build"); '
              'import gate_plan,landing; root=Path.cwd(); '
              'metadata=json.loads(subprocess.check_output(["cargo","metadata","--locked","--no-deps","--format-version","1"])); '
              'print(json.dumps(gate_plan.commands(sys.argv[1],metadata["packages"],landing.paths(root,sys.argv[2],sys.argv[3]),root,base=sys.argv[2])))')
    record['commands'] = json.loads(command(root, sys.executable, '-c', script,
                                          'full' if full else 'affected', base, accepted,
                                          env=isolated_environment()))
    path.write_text(json.dumps(record))
    return request


def notice_base(canonical, root):
    (canonical / NOTICE).write_text(BASE)
    base = commit(canonical, [NOTICE])
    command(root, 'git', 'fetch', '-q', str(canonical), 'main')
    command(root, 'git', 'reset', '--hard', 'FETCH_HEAD')
    return base


def edit(root, old, new):
    path = root / NOTICE
    path.write_text(path.read_text().replace(old, new))
    return commit(root, [NOTICE])


class DocumentCompositionTests(unittest.TestCase):
    def test_nonoverlapping_notices_compose_run_fresh_checks_and_verify(self):
        with fixture() as (outer, canonical, root, _, env):
            base = notice_base(canonical, root)
            accepted = edit(root, 'First notice', 'Candidate attribution')
            advanced = edit(canonical, 'Second notice', 'Canonical attribution')
            request = request_input(outer, root, base, accepted)
            original = Path(request['gate_log']).with_suffix('.log.receipt.json').read_bytes()
            result = invoke(root, request, env)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            record = json.loads(Path(request['receipt']).read_text())
            self.assertEqual(record['merged_documents'], [NOTICE])
            self.assertEqual(command(root, "git", "show", record["candidate"] + ":" + NOTICE), BASE.replace('First notice', 'Candidate attribution')
                             .replace('Second notice', 'Canonical attribution').strip())
            # These are actual new gate executions, not just rewritten receipt fields.
            log = Path(request['receipt']).with_suffix('.log').read_text()
            self.assertIn('cargo test --locked -p grapt-types', log)
            self.assertEqual(record['qualification_mode'], 'affected')
            self.assertTrue(record['qualification_receipt_sha256'])
            self.assertIn('# command_seconds:', log)
            self.assertTrue(record['commands'])
            self.assertEqual(original, Path(request['gate_log']).with_suffix('.log.receipt.json').read_bytes())
            request.update(candidate=record['candidate'], new_base=advanced)
            verified = invoke(root, request, env, 'verify')
            self.assertEqual(verified.returncode, 0, verified.stdout + verified.stderr)
            # Relabelling the source identity cannot approve an arbitrary notice edit.
            with exported_checkout(root, request):
                candidate = edit(root, 'Candidate attribution', 'Unreviewed attribution')
            request['candidate'] = candidate
            record.update(candidate=candidate, source_before={'commit': candidate, 'clean': True},
                          source_after={'commit': candidate, 'clean': True})
            Path(request['receipt']).write_text(json.dumps(record))
            rejected = invoke(root, request, env, 'verify')
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn('approved document merge: ' + NOTICE, rejected.stderr)

    def test_runtime_failure_only_in_merged_tree_prevents_receipt(self):
        with fixture() as (outer, canonical, root, _, env):
            source = canonical / 'a/src/lib.rs'
            source.write_text('#[test]\nfn notices_remain_compatible() {\n'
                              '    let text = include_str!("../../THIRD_PARTY_NOTICES.md");\n'
                              '    assert!(!(text.contains("Candidate attribution") && text.contains("Canonical attribution")));\n}\n')
            command(canonical, 'cargo', 'fmt', '-p', 'a')
            commit(canonical, ['a/src/lib.rs'])
            base = notice_base(canonical, root)
            edit(root, 'First notice', 'Candidate attribution')
            with (root / 'a/src/lib.rs').open('a') as output:
                output.write('// Candidate owns this direct consumer.\n')
            command(root, 'cargo', 'fmt', '-p', 'a')
            accepted = commit(root, ['a/src/lib.rs'])
            edit(canonical, 'Second notice', 'Canonical attribution')
            for tree in (root, canonical):
                command(tree, 'cargo', 'test', '--locked', '-p', 'a', env=env)
            request = request_input(outer, root, base, accepted)
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('notices_remain_compatible ... FAILED', result.stderr)
            self.assertFalse(Path(request['receipt']).exists())

    def test_tier_one_document_overlap_runs_and_verifies_full_qualification(self):
        with fixture() as (outer, canonical, root, _, env):
            base = notice_base(canonical, root)
            accepted = edit(root, 'First notice', 'Candidate attribution')
            advanced = edit(canonical, 'Second notice', 'Canonical attribution')
            request = request_input(outer, root, base, accepted, full=True)
            result = invoke(root, request, env)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            record = json.loads(Path(request['receipt']).read_text())
            self.assertEqual(record['qualification_mode'], 'full')
            self.assertIn('cargo test --locked --workspace', Path(request['receipt']).with_suffix('.log').read_text())
            request.update(candidate=record['candidate'], new_base=advanced)
            verified = invoke(root, request, env, 'verify')
            self.assertEqual(verified.returncode, 0, verified.stdout + verified.stderr)
            record['qualification_mode'] = 'composition'
            Path(request['receipt']).write_text(json.dumps(record))
            rejected = invoke(root, request, env, 'verify')
            self.assertNotEqual(rejected.returncode, 0)
            self.assertIn('fresh qualification for its tier', rejected.stderr)

    def test_failure_in_fresh_composed_tree_check_prevents_receipt(self):
        with fixture() as (outer, canonical, root, _, env):
            base = notice_base(canonical, root)
            accepted = edit(root, 'First notice', 'Candidate attribution')
            edit(canonical, 'Second notice', 'Canonical attribution')
            # The original candidate was valid; the new canonical test graph is not.
            (canonical / 'grapt-types/tests/no_god_files.rs').write_text('compile_error!("fresh tree failure");\n')
            commit(canonical, ['grapt-types/tests/no_god_files.rs'])
            request = request_input(outer, root, base, accepted)
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('fresh tree failure', result.stderr)
            self.assertFalse(Path(request['receipt']).exists())

    def test_conflicting_notice_hunks_refuse_by_name_before_rebase(self):
        with fixture() as (outer, canonical, root, _, env):
            base = notice_base(canonical, root)
            accepted = edit(root, 'First notice', 'Candidate attribution')
            edit(canonical, 'First notice', 'Conflicting attribution')
            request = request_input(outer, root, base, accepted)
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('conflicting hunks: ' + NOTICE, result.stderr)
            self.assertEqual(command(root, 'git', 'rev-parse', 'HEAD'), accepted)
            self.assertEqual(command(root, 'git', 'status', '--porcelain'), '')
            self.assertFalse(Path(request['receipt']).exists())

    def test_code_overlap_still_refuses_even_for_nonoverlapping_hunks(self):
        with fixture() as (outer, canonical, root, base, env):
            path = 'a/src/lib.rs'
            (root / path).write_text('// Candidate comment\npub fn value() -> u32 { 1 }\n')
            accepted = commit(root, [path])
            (canonical / path).write_text('pub fn value() -> u32 { 1 }\n// Canonical comment\n')
            commit(canonical, [path])
            result = invoke(root, request_input(outer, root, base, accepted), env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('overlaps candidate path: ' + path, result.stderr)
            self.assertEqual(command(root, 'git', 'rev-parse', 'HEAD'), accepted)

    def test_document_type_encoding_and_size_are_bounded(self):
        with fixture() as (_, canonical, root, _, _):
            base = notice_base(canonical, root)
            for content, message in [(b'\x00binary', 'binary'), (b'\xff', 'UTF-8'),
                                     (b'x' * (1024 * 1024 + 1), 'exceeds 1 MiB')]:
                with self.subTest(message=message):
                    (root / NOTICE).write_bytes(content)
                    candidate = commit(root, [NOTICE])
                    with self.assertRaisesRegex(ValueError, message):
                        landing.document_blob(root, candidate, NOTICE)
            (root / NOTICE).chmod(0o755)
            candidate = commit(root, [NOTICE])
            with self.assertRaisesRegex(ValueError, 'regular document'):
                landing.document_blob(root, candidate, NOTICE)
            with self.assertRaisesRegex(ValueError, 'regular document'):
                landing.document_blob(root, base, 'missing.md')

    def test_fresh_qualification_preserves_preexisting_logs(self):
        with fixture() as (outer, canonical, root, _, env):
            base = notice_base(canonical, root)
            accepted = edit(root, 'First notice', 'Candidate attribution')
            edit(canonical, 'Second notice', 'Canonical attribution')
            request = request_input(outer, root, base, accepted)
            log = Path(request['receipt']).with_suffix('.log')
            log.write_text('Earlier result must survive\n')
            result = invoke(root, request, env)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(log.read_text(), 'Earlier result must survive\n')
            self.assertFalse(Path(request['receipt']).exists())

    def test_allowlist_does_not_expand_to_other_documents_or_case_aliases(self):
        for path in ('docs/help.md', 'README.md', NOTICE.lower()):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, 'overlaps'):
                landing.assert_disjoint({path}, {path})
        with self.assertRaisesRegex(ValueError, 'overlaps'):
            landing.assert_disjoint({NOTICE}, {NOTICE + '/child'})


if __name__ == '__main__':
    unittest.main()
