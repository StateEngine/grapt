"""Witnesses for the provider mutation runner's target isolation.

Seven properties are witnessed, and each one is re-proved by deleting the code
that carries it and watching its named witness fail (`ResurrectionTest`).
Without the resurrection half, a witness can pass while asserting something no
line of the runner is responsible for.

The witnesses are hermetic on purpose: no cargo, no git, no network. A cargo
child is simulated by a stand-in that writes into every target directory it can
still address -- the `--target-dir` it was handed, `CARGO_TARGET_DIR`,
`CARGO_BUILD_TARGET_DIR`, and its own `cwd/target`. That is the whole threat
model in one object: if the runner leaves any of those addressable, the
stand-in writes there and the witness fails.
"""

import importlib.util
import io
import json
import os
import tempfile
import unittest
from pathlib import Path


RUNNER = Path(__file__).with_name("run.py")
HOSTILE_MARKER = "artifact-from-a-previous-mutant"


def write_text(path, text, newline="\n"):
    with io.open(path, "w", encoding="utf-8", newline=newline) as handle:
        handle.write(text)


def runner_source():
    with io.open(RUNNER, encoding="utf-8") as handle:
        return handle.read()


def subprocess_owners(source):
    owners = {}
    current = None
    for line in source.splitlines():
        if line.startswith("def "):
            current = line[4:].split("(", 1)[0]
        if "subprocess.run(" in line:
            owners[current] = owners.get(current, 0) + 1
    return owners


# A mutated runner is loaded from a file, so its directory must outlive the
# module object that was compiled from it. They are released together at the
# end of the module rather than at an interpreter shutdown nobody watches.
MUTATED_ROOTS = []


def tearDownModule():
    while MUTATED_ROOTS:
        MUTATED_ROOTS.pop().cleanup()


def load_runner(source=None):
    """Load run.py, optionally from mutated source, as a private module."""
    if source is None:
        path = RUNNER
    else:
        keepalive = tempfile.TemporaryDirectory()
        MUTATED_ROOTS.append(keepalive)
        path = Path(keepalive.name) / "run.py"
        write_text(path, source)
    spec = importlib.util.spec_from_file_location("grapt_provider_mutations", path)
    if spec is None or spec.loader is None:
        raise RuntimeError("provider mutation runner could not be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CargoStandIn:
    """A cargo that honours every target address it is still given."""

    def __init__(self, returncode=101, stdout="0 passed; 1 failed", stderr=""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr
        self.argv = None
        self.env = None
        self.cwd = None
        self.wrote = []

    def __call__(
        self,
        argv,
        cwd=None,
        capture_output=None,
        text=None,
        check=None,
        env=None,
        timeout_seconds=None,
    ):
        self.argv = list(argv)
        self.env = dict(env or {})
        self.cwd = cwd
        addresses = []
        if "--target-dir" in self.argv:
            addresses.append(self.argv[self.argv.index("--target-dir") + 1])
        for name in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"):
            if name in self.env:
                addresses.append(self.env[name])
        if not addresses:
            addresses.append(os.path.join(cwd, "target"))
        for address in addresses:
            os.makedirs(address, exist_ok=True)
            write_text(os.path.join(address, "debug-artifact"), "built")
            if address not in self.wrote:
                self.wrote.append(address)
        return CargoStandIn.Result(self.returncode, self.stdout, self.stderr)

    class Result:
        def __init__(self, returncode, stdout, stderr):
            self.returncode = returncode
            self.stdout = stdout
            self.stderr = stderr


def restore_variable(name, previous):
    if previous is None:
        os.environ.pop(name, None)
    else:
        os.environ[name] = previous


def hostile_environment(case, hostile):
    """Set both ambient target variables, and forge an artifact inside them."""
    os.makedirs(hostile, exist_ok=True)
    write_text(os.path.join(hostile, HOSTILE_MARKER), "stale")
    for name in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"):
        previous = os.environ.get(name)
        os.environ[name] = hostile
        case.addCleanup(restore_variable, name, previous)


# --- the witnesses ---------------------------------------------------------
# Each takes the module under test, so the resurrection pass can run the exact
# same assertions against a deliberately broken runner.


def witness_ambient_is_stripped_not_merely_overridden(case, runner):
    """W1: the cargo child is addressed by flag AND by a cleaned environment."""
    with tempfile.TemporaryDirectory() as directory:
        hostile = os.path.join(directory, "ambient-cargo-target")
        hostile_environment(case, hostile)
        mine = os.path.join(directory, "targets", "Z01")
        workspace = os.path.join(directory, "workspace")
        os.makedirs(workspace)
        cargo = CargoStandIn()
        runner.subprocess.run = cargo
        runner.run_cargo(
            ["cargo", "test", "-p", "grapt-provider", "--", "--exact", "a::b"],
            workspace,
            mine,
        )
        case.assertIn("--target-dir", cargo.argv)
        case.assertEqual(cargo.argv[cargo.argv.index("--target-dir") + 1], mine)
        # Before the filter separator, or cargo reads it as a test-binary flag.
        case.assertLess(cargo.argv.index("--target-dir"), cargo.argv.index("--"))
        case.assertEqual(cargo.env.get("CARGO_TARGET_DIR"), mine)
        case.assertNotIn("CARGO_BUILD_TARGET_DIR", cargo.env)


def witness_hostile_ambient_receives_zero_writes(case, runner):
    """W2: nothing this runner starts can address the ambient target."""
    with tempfile.TemporaryDirectory() as directory:
        hostile = os.path.join(directory, "ambient-cargo-target")
        hostile_environment(case, hostile)
        mine = os.path.join(directory, "targets", "Z01")
        workspace = os.path.join(directory, "workspace")
        os.makedirs(workspace)
        cargo = CargoStandIn()
        runner.subprocess.run = cargo
        before = runner.directory_fingerprint(hostile)
        runner.run_cargo(
            ["cargo", "test", "-p", "grapt-provider", "--", "--exact", "a::b"],
            workspace,
            mine,
        )
        case.assertEqual(runner.directory_fingerprint(hostile), before)
        case.assertEqual(sorted(os.listdir(hostile)), [HOSTILE_MARKER])
        case.assertEqual(cargo.wrote, [mine])
        case.assertFalse(os.path.exists(os.path.join(workspace, "target")))


def witness_recursive_ambient_fingerprint_detects_nested_write(case, runner):
    """W7: a realistic write below debug/deps disturbs ambient custody."""
    with tempfile.TemporaryDirectory() as directory:
        hostile = os.path.join(directory, "ambient-cargo-target")
        hostile_environment(case, hostile)
        nested = os.path.join(hostile, "debug", "deps")
        os.makedirs(nested)
        write_text(os.path.join(nested, "existing"), "seed")
        before = runner.directory_fingerprint(hostile)
        top_level = sorted(os.listdir(hostile))
        write_text(os.path.join(nested, "new-artifact"), "x" * 4096)
        case.assertEqual(sorted(os.listdir(hostile)), top_level)
        case.assertNotEqual(runner.directory_fingerprint(hostile), before)


def witness_each_row_owns_a_deterministic_directory(case, runner):
    """W3: per-row, stable in the row id, and inside the run root."""
    with tempfile.TemporaryDirectory() as run_root:
        first = runner.target_dir_for(run_root, "Z01")
        again = runner.target_dir_for(run_root, "Z01")
        second = runner.target_dir_for(run_root, "PR07")
        case.assertEqual(first, again)
        case.assertNotEqual(first, second)
        case.assertEqual(os.path.basename(first), "Z01")
        for directory in (first, second):
            case.assertTrue(runner.contained(run_root, directory))
        for rejected in ("", "..", "Z01/../../escape", "no-digits"):
            with case.assertRaises(runner.GateFailure):
                runner.target_dir_for(run_root, rejected)


# --- the row-level backstops, witnessed on run_one itself -------------------

ROW = {
    "id": "Z01",
    "package": "grapt-provider",
    "file": "crates/grapt-provider/src/codex/body.rs",
    "test": "codex::body::tests::wire_keys",
    "old": "KEEP",
    "new": "DROP",
}


def fake_git_that_makes_a_workspace(source_by_path):
    def git(*args, cwd, check=True):
        if args[0] == "worktree" and args[1] == "add":
            workspace = args[4]
            for relative, text in source_by_path.items():
                path = os.path.join(workspace, relative.replace("/", os.sep))
                os.makedirs(os.path.dirname(path), exist_ok=True)
                write_text(path, text)
        return ""

    return git


def run_row(runner, run_root, cargo_effect):
    runner.git = fake_git_that_makes_a_workspace({ROW["file"]: "fn a() { KEEP }\n"})
    runner.run_cargo = cargo_effect
    return runner.run_one(dict(ROW), "repo", "candidate", run_root)


def cargo_result(returncode=101):
    return CargoStandIn.Result(returncode, "1 passed; 1 failed", "panicked at src/lib.rs:1:1")


def witness_a_row_that_built_elsewhere_cannot_report_red(case, runner):
    """W4: no artifacts of our own means no verdict, whatever cargo said."""

    def built_nowhere_of_ours(command, cwd, target_dir, timeout_seconds=None):
        return cargo_result()

    with tempfile.TemporaryDirectory() as run_root:
        accounted, label, _ = run_row(runner, run_root, built_nowhere_of_ours)
        case.assertFalse(accounted)
        case.assertEqual(label, "ROW BUILT OUTSIDE ITS OWN TARGET DIRECTORY")


def witness_a_target_inside_the_worktree_is_refused(case, runner):
    """W5: the disposable worktree is not a target directory either."""

    def built_in_the_worktree(command, cwd, target_dir, timeout_seconds=None):
        os.makedirs(target_dir, exist_ok=True)
        write_text(os.path.join(target_dir, "debug-artifact"), "x")
        os.makedirs(os.path.join(cwd, "target"), exist_ok=True)
        return cargo_result()

    with tempfile.TemporaryDirectory() as run_root:
        accounted, label, _ = run_row(runner, run_root, built_in_the_worktree)
        case.assertFalse(accounted)
        case.assertEqual(label, "TARGET LEAKED INTO THE WORKTREE")


def witness_a_pre_existing_target_directory_is_refused(case, runner):
    """W6: a row never inherits a directory it did not create."""

    def unreached(command, cwd, target_dir, timeout_seconds=None):
        raise AssertionError("cargo must not start against a dirty target directory")

    with tempfile.TemporaryDirectory() as run_root:
        os.makedirs(os.path.join(run_root, "targets", ROW["id"]))
        with case.assertRaises(runner.GateFailure):
            run_row(runner, run_root, unreached)


WITNESSES = {
    "W1-ambient-stripped": witness_ambient_is_stripped_not_merely_overridden,
    "W2-zero-writes": witness_hostile_ambient_receives_zero_writes,
    "W7-recursive-custody": witness_recursive_ambient_fingerprint_detects_nested_write,
    "W3-per-row-deterministic": witness_each_row_owns_a_deterministic_directory,
    "W4-built-elsewhere-refused": witness_a_row_that_built_elsewhere_cannot_report_red,
    "W5-worktree-target-refused": witness_a_target_inside_the_worktree_is_refused,
    "W6-dirty-target-refused": witness_a_pre_existing_target_directory_is_refused,
}


class TargetIsolationTest(unittest.TestCase):
    """The witnesses, run against the committed runner."""

    def test_every_witness_holds(self):
        for name, witness in WITNESSES.items():
            with self.subTest(witness=name):
                witness(self, load_runner())

    def test_a_completed_row_relinquishes_its_target_directory(self):
        runner = load_runner()

        def built_in_its_own_target(command, cwd, target_dir, timeout_seconds=None):
            os.makedirs(target_dir, exist_ok=True)
            write_text(os.path.join(target_dir, "debug-artifact"), "x")
            return CargoStandIn.Result(101, "0 passed; 1 failed", "named assertion")

        with tempfile.TemporaryDirectory() as run_root:
            accounted, label, _ = run_row(
                runner, run_root, built_in_its_own_target
            )
            self.assertTrue(accounted)
            self.assertEqual(label, "RED (witness fired)")
            self.assertFalse(
                os.path.exists(os.path.join(run_root, "targets", ROW["id"]))
            )


class SeamTest(unittest.TestCase):
    def test_exact_test_accounting_distinguishes_zero_one_and_many(self):
        runner = load_runner()
        self.assertEqual(runner.test_count("0 passed; 0 failed"), 0)
        self.assertEqual(runner.test_count("0 passed; 1 failed"), 1)
        self.assertEqual(runner.test_count("1 passed; 1 failed"), 2)
        self.assertEqual(runner.listed_test_count("a::b: test\n"), 1)
        self.assertEqual(runner.listed_test_count("a::b: test\nc::d: test\n"), 2)

    def test_provider_r3_pack_selects_its_exact_order_and_refuses_bad_membership(self):
        runner = load_runner()
        with io.open(runner.MANIFEST, encoding="utf-8") as handle:
            manifest = json.load(handle)
        selected, problem = runner.select_pack(
            manifest["mutations"], manifest["packs"], "PROVIDER-1-R3"
        )
        self.assertIsNone(problem)
        self.assertEqual(
            [row["id"] for row in selected],
            [f"Z{number}" for number in range(18, 32)],
        )

        rows = [{"id": "Z18"}, {"id": "Z19"}]
        for packs, expected in (
            ({"bad": []}, "must declare at least one"),
            ({"bad": ["Z18", "Z18"]}, "duplicate"),
            ({"bad": ["Z20"]}, "missing"),
        ):
            with self.subTest(expected=expected):
                refused, detail = runner.select_pack(rows, packs, "bad")
                self.assertIsNone(refused)
                self.assertIn(expected, detail)

    def test_the_seam_refuses_a_caller_that_names_its_own_target(self):
        runner = load_runner()
        with self.assertRaises(runner.GateFailure):
            runner.run_cargo(["cargo", "test", "--target-dir", "elsewhere"], ".", "mine")

    def test_the_seam_refuses_a_non_cargo_command(self):
        runner = load_runner()
        with self.assertRaises(runner.GateFailure):
            runner.run_cargo(["rustc", "--version"], ".", "mine")

    def test_the_seam_refuses_target_custody_in_environment_updates(self):
        runner = load_runner()
        with self.assertRaises(runner.GateFailure):
            runner.run_cargo(
                ["cargo", "test"],
                ".",
                "mine",
                {"CARGO_TARGET_DIR": "elsewhere"},
            )

    def test_rustdoc_uses_the_cargo_seam_with_its_lint_environment(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory() as directory:
            workspace = os.path.join(directory, "workspace")
            target = os.path.join(directory, "targets", "H02")
            os.makedirs(workspace)
            expected = "error: public documentation for `AttemptCustody` links to private item"
            cargo = CargoStandIn(
                stdout="",
                stderr=(
                    f"{expected}\n{runner.RUSTDOC_LINT_NOTE}\n"
                    f"{runner.RUSTDOC_TAIL_ERROR}\n"
                )
            )
            runner.subprocess.run = cargo
            accounted, label, detail = runner.run_rustdoc(
                {"package": runner.RUSTDOC_PACKAGE, "expect_error": expected},
                workspace,
                target,
            )
            self.assertTrue(accounted)
            self.assertEqual(label, "RED (rustdoc witness fired)")
            self.assertEqual(detail, expected)
            self.assertEqual(cargo.env.get("RUSTDOCFLAGS"), runner.RUSTDOC_FLAGS)
            self.assertEqual(cargo.wrote, [target])
            self.assertIn("--target-dir", cargo.argv)

    def test_no_cargo_child_is_started_outside_the_seam(self):
        """The invariant is only as complete as the number of seams.

        Any dispatch this runner grows later -- a rustdoc row, a clippy row --
        must go through `run_cargo` too. A `subprocess.run` anywhere but the
        two seams that own one is a defect named here at authoring time
        instead of at gate time.
        """
        self.assertEqual(subprocess_owners(runner_source()), {"git": 1, "run_cargo": 1})

    def test_rustdoc_direct_dispatch_resurrection_is_killed(self):
        source = runner_source()
        old = "    result = run_cargo(\n        list(RUSTDOC_COMMAND),"
        new = "    result = subprocess.run(\n        list(RUSTDOC_COMMAND),"
        self.assertEqual(source.count(old), 1)
        mutated = source.replace(old, new, 1)
        with self.assertRaises(AssertionError):
            self.assertEqual(
                subprocess_owners(mutated),
                {"git": 1, "run_cargo": 1},
            )


class AmbientCustodyTest(unittest.TestCase):
    def test_a_nested_write_into_the_ambient_target_is_visible_to_the_battery_check(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory() as directory:
            hostile = os.path.join(directory, "ambient-cargo-target")
            hostile_environment(self, hostile)
            nested = os.path.join(hostile, "debug", "deps")
            os.makedirs(nested)
            write_text(os.path.join(nested, "existing"), "seed")
            ambient = runner.ambient_targets()
            self.assertEqual(sorted(ambient), ["CARGO_BUILD_TARGET_DIR", "CARGO_TARGET_DIR"])
            before = runner.ambient_fingerprints(ambient)
            top_level = sorted(os.listdir(hostile))
            write_text(os.path.join(nested, "new-artifact"), "x" * 4096)
            self.assertEqual(sorted(os.listdir(hostile)), top_level)
            self.assertNotEqual(before, runner.ambient_fingerprints(ambient))

    def test_an_absent_ambient_target_is_not_created_by_looking(self):
        runner = load_runner()
        with tempfile.TemporaryDirectory() as directory:
            missing = os.path.join(directory, "never-created")
            self.assertEqual(runner.directory_fingerprint(missing), ("absent",))
            self.assertFalse(os.path.exists(missing))


# --- resurrection: delete the invariant, watch the named witness fail -------

RESURRECTIONS = (
    (
        "W2-zero-writes",
        "the ambient variables are merely overridden, not removed",
        "    for name in AMBIENT_TARGET_VARS:\n        environment.pop(name, None)\n",
        "",
    ),
    (
        "W1-ambient-stripped",
        "the child is addressed by environment alone, with no --target-dir",
        '    separator = argv.index("--") if "--" in argv else len(argv)\n'
        '    argv[separator:separator] = ["--target-dir", target_dir]\n',
        "",
    ),
    (
        "W7-recursive-custody",
        "the ambient fingerprint stops at the top level",
        '                if kind == "directory":\n'
        "                    pending.append((relative, entry.path))\n",
        "",
    ),
    (
        "W3-per-row-deterministic",
        "every row shares one target directory again",
        '    directory = os.path.join(run_root, "targets", mutation_id)',
        '    directory = os.path.join(run_root, "targets")',
    ),
    (
        "W4-built-elsewhere-refused",
        "a row may report a verdict without owning any artifacts",
        '        if directory_fingerprint(target_dir) == ("absent",):\n'
        '            return False, "ROW BUILT OUTSIDE ITS OWN TARGET DIRECTORY", target_dir\n',
        "",
    ),
    (
        "W5-worktree-target-refused",
        "a target inside the disposable worktree is accepted again",
        '        leaked = os.path.join(workspace, "target")\n'
        "        if os.path.exists(leaked):\n"
        '            return False, "TARGET LEAKED INTO THE WORKTREE", leaked\n',
        "",
    ),
    (
        "W6-dirty-target-refused",
        "a row inherits whatever is already in its target directory",
        "    if os.path.exists(target_dir):\n"
        "        raise GateFailure(\n"
        "            f\"{entry['id']}: its target directory already exists before the row ran: \"\n"
        "            f\"{target_dir}\"\n"
        "        )\n",
        "",
    ),
)


class ResurrectionTest(unittest.TestCase):
    def test_every_resurrection_applies_exactly_once(self):
        source = runner_source()
        for witness, description, old, new in RESURRECTIONS:
            with self.subTest(witness=witness, defect=description):
                self.assertEqual(source.count(old), 1, description)
                self.assertNotEqual(old, new)

    def test_every_resurrection_is_killed_by_its_named_witness(self):
        source = runner_source()
        for witness, description, old, new in RESURRECTIONS:
            with self.subTest(witness=witness, defect=description):
                mutated = load_runner(source.replace(old, new, 1))
                with self.assertRaises(AssertionError, msg=description):
                    WITNESSES[witness](self, mutated)

    def test_the_witness_set_covers_every_resurrection_exactly(self):
        named = {witness for witness, _, _, _ in RESURRECTIONS}
        self.assertEqual(named, set(WITNESSES))


if __name__ == "__main__":
    unittest.main()
