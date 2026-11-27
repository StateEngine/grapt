import importlib.util
import subprocess
import tempfile
from unittest import mock
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("gate_plan", Path(__file__).with_name("gate_plan.py"))
gate_plan = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate_plan)


def source(lines, trailing_newline=True):
    body = "\n".join(f"line {index:06d}" for index in range(lines))
    if trailing_newline:
        body += "\n"
    return body.encode()


class PhysicalLineCountTests(unittest.TestCase):
    def test_counts_lf_crlf_and_lone_cr_like_the_rust_law(self):
        self.assertEqual(gate_plan.physical_line_count(b"first\nsecond\nthird\n"), 3)
        self.assertEqual(gate_plan.physical_line_count(b"first\r\nsecond\r\nthird\r\n"), 3)
        self.assertEqual(gate_plan.physical_line_count(b"first\rsecond\rthird"), 3)
        self.assertEqual(gate_plan.physical_line_count(b""), 0)
        self.assertEqual(gate_plan.physical_line_count(b"one"), 1)
        self.assertEqual(gate_plan.physical_line_count(b"one\ntwo"), 2)


class GodFileAdvisoryTests(unittest.TestCase):
    def setUp(self):
        self._directory = tempfile.TemporaryDirectory()
        self.addCleanup(self._directory.cleanup)
        self.root = Path(self._directory.name)
        self.git('init', '-q', str(self.root))
        self.base = None

    def git(self, *args):
        return subprocess.run(['git', *args], cwd=self.root, check=True,
                              capture_output=True)

    def commit(self, relpath, data):
        path = self.root / relpath
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        self.git('add', '--', relpath)
        self.git('-c', 'user.name=Gate', '-c', 'user.email=gate@localhost',
                 'commit', '-q', '-m', 'fixture')
        self.base = self.git('rev-parse', 'HEAD').stdout.decode().strip()

    def test_a_seven_hundred_line_file_is_not_named(self):
        self.commit('crates/x/src/y.rs', source(700))
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base), [])

    def test_seven_hundred_and_one_lines_get_one_line(self):
        self.commit('crates/x/src/y.rs', source(700))
        (self.root / 'crates/x/src/y.rs').write_bytes(source(701))
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base),
            ['crates/x/src/y.rs: 701 lines (+1 in this change), over 700: plan a split'])

    def test_a_file_without_a_trailing_newline_counts_its_last_line(self):
        self.commit('crates/x/src/y.rs', source(700, trailing_newline=False))
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base), [])
        (self.root / 'crates/x/src/y.rs').write_bytes(source(701, trailing_newline=False))
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base),
            ['crates/x/src/y.rs: 701 lines (+1 in this change), over 700: plan a split'])

    def test_a_new_file_names_all_of_its_lines(self):
        self.commit('crates/x/src/other.rs', source(10))
        (self.root / 'crates/x/src/y.rs').write_bytes(source(812))
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base),
            ['crates/x/src/y.rs: 812 lines (+812 in this change), over 700: plan a split'])

    def test_a_shrunken_file_keeps_its_advisory_with_a_negative_delta(self):
        self.commit('crates/x/src/y.rs', source(812))
        (self.root / 'crates/x/src/y.rs').write_bytes(source(800))
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base),
            ['crates/x/src/y.rs: 800 lines (-12 in this change), over 700: plan a split'])

    def test_an_unchanged_line_count_says_unchanged(self):
        self.commit('crates/x/src/y.rs', source(812))
        changed = source(812).replace(b'line 000000', b'changed line 000000')
        self.assertEqual(len(changed.splitlines()), 812)
        (self.root / 'crates/x/src/y.rs').write_bytes(changed)
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base),
            ['crates/x/src/y.rs: 812 lines (line count unchanged in this change), over 700: plan a split'])

    def test_a_deleted_file_is_silent(self):
        self.commit('crates/x/src/y.rs', source(812))
        (self.root / 'crates/x/src/y.rs').unlink()
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base), [])

    def test_paths_outside_crates_and_non_rust_paths_are_ignored(self):
        self.commit('bench/big.rs', source(812))
        self.commit('crates/x/src/y.txt', source(812))
        self.assertEqual(
            gate_plan.god_file_advisories({'bench/big.rs', 'crates/x/src/y.txt'},
                                          self.root, self.base), [])

    def test_a_missing_base_prints_nothing(self):
        self.commit('crates/x/src/y.rs', source(812))
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, '0' * 40), [])

    def test_a_base_resolution_timeout_prints_nothing(self):
        self.commit('crates/x/src/y.rs', source(812))
        def timed_out(*args, **kwargs):
            raise subprocess.TimeoutExpired(cmd=['git', 'rev-parse'], timeout=10)
        with mock.patch.object(gate_plan.subprocess, 'check_output', side_effect=timed_out):
            self.assertEqual(
                gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base), [])

    def test_a_baseline_read_timeout_prints_nothing(self):
        self.commit('crates/x/src/y.rs', source(812))
        def timed_out(*args, **kwargs):
            raise subprocess.TimeoutExpired(cmd=['git', 'ls-tree'], timeout=10)
        with mock.patch.object(gate_plan.subprocess, 'run', side_effect=timed_out):
            self.assertEqual(
                gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base), [])

    def test_an_unreadable_baseline_is_not_reported_as_new(self):
        self.commit('crates/x/src/y.rs', source(812))
        blob = self.git('rev-parse', 'HEAD:crates/x/src/y.rs').stdout.decode().strip()
        object_file = self.root / '.git' / 'objects' / blob[:2] / blob[2:]
        object_file.chmod(0o644)
        object_file.write_bytes(b'corrupt')
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/x/src/y.rs'}, self.root, self.base), [])

    def test_a_non_repository_root_prints_nothing(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            target = root / 'crates/x/src/y.rs'
            target.parent.mkdir(parents=True)
            target.write_bytes(source(812))
            self.assertEqual(
                gate_plan.god_file_advisories({'crates/x/src/y.rs'}, root, 'base'), [])

    def test_advisories_print_in_sorted_order(self):
        self.commit('crates/z/src/b.rs', source(701))
        self.commit('crates/a/src/a.rs', source(705))
        self.assertEqual(
            gate_plan.god_file_advisories({'crates/z/src/b.rs', 'crates/a/src/a.rs'},
                                          self.root, self.base),
            ['crates/a/src/a.rs: 705 lines (line count unchanged in this change), over 700: plan a split',
             'crates/z/src/b.rs: 701 lines (line count unchanged in this change), over 700: plan a split'])


if __name__ == '__main__':
    unittest.main()
