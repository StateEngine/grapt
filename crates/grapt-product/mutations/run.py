import json
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
MANIFEST = Path(__file__).with_name("manifest.json")


def run(args, cwd=ROOT, env=None):
    return subprocess.run(
        args, cwd=cwd, text=True, capture_output=True, check=False, env=env
    )


def git_blob(head, relative):
    """Read a Git blob as bytes, independent of the host's text locale.

    Windows resolves ``text=True`` through its preferred encoding. On this
    fleet host that is CP1252, so a UTF-8 em dash or section sign is decoded to
    different Unicode text than ``Path.read_text(encoding="utf-8")``. The old
    residue backstop therefore refused a byte-identical tree. Blob custody is a
    byte contract; keep both sides binary and normalize only checked-in CRLF.
    """
    return subprocess.run(
        ["git", "show", f"{head}:{relative}"],
        cwd=ROOT,
        capture_output=True,
        check=False,
    )


def worktree_env(path):
    """Pin CARGO_TARGET_DIR INSIDE the disposable worktree, unconditionally.

    The governed profiles set CARGO_TARGET_DIR to a per-domain cache shared by
    every step, and this runner used to inherit it. Cargo then keyed one set of
    artifacts by package name across worktrees, so a mutant built in one
    disposable tree could be re-executed for the next row: a VERIFIED-CLEAN
    worktree fails carrying the previous mutant's evidence, which scores as that
    row's red and proves nothing. Measured at 8746cde4: same clean tree, same
    exact filter - shared target FAILED with the prior mutant's panic, a
    per-worktree target PASSED, and CARGO_TARGET_DIR was the only variable.

    Pinning is unconditional on purpose. An advisory or opt-in isolation is one
    forgotten export away from the same silent contamination, and the failure is
    invisible: it looks exactly like the row doing its job.
    """
    environment = dict(os.environ)
    environment["CARGO_TARGET_DIR"] = str(path / "target")
    return environment


# Every id is one alphabetic token followed by digits. Selection compares that
# whole token, so `FS` can never draw an `FSX`-style row and `IPR` can never be
# drawn by a bare `I`. Each inventory below is exact and independent: extending
# one cannot silently shrink the other.
EXACT_INVENTORIES = {
    "IPR": [f"IPR{number:02d}" for number in range(1, 23)],
    "DGH": [f"DGH{number:02d}" for number in (1, 2, 3, 4, 5, 6, 9)],
    "RMH": [f"RMH{number:02d}" for number in range(1, 3)],
    "FS": [f"FS{number:02d}" for number in range(1, 5)],
    "LH": [f"LH{number:02d}" for number in range(1, 22)],
    "DTV": [f"DTV{number:02d}" for number in range(1, 14)],
    "PC": [f"PC{number:02d}" for number in range(1, 6)],
    "PU": [f"PU{number:02d}" for number in range(1, 11)],
    "PCU": [f"PCU{number:02d}" for number in range(1, 20)],
    "RDT": [f"RDT{number:02d}" for number in range(1, 8)],
    "SCO": [f"SCO{number:02d}" for number in range(1, 5)],
    "DDR": [f"DDR{number:02d}" for number in range(1, 11)],
}
# The governed profiles invoke this runner with NO argument. That default used to
# mean "IPR only", so a full gate could pass while FS01-FS04 were never executed:
# the rows existed, were selectable by hand, and no governed route ever drew them.
# The default is now every ruled family in this exact order, so skipping a family
# is not a thing a profile edit can express. Order is pinned by the census test
# with a reversible counterexample; it is data, not an accident of dict order.
RULED_ORDER = ("IPR", "DGH", "RMH", "FS", "LH", "DTV", "PC", "PU", "PCU", "RDT", "SCO", "DDR")
REFUSAL_PREFIX = "IPR"


def id_token(value):
    for index, character in enumerate(value):
        if character.isdigit():
            return value[:index]
    return value


def fail(message):
    print(f"{REFUSAL_PREFIX} REFUSED: {message}", file=sys.stderr)
    raise SystemExit(2)


def validate_cargo_row(row):
    """Refuse cargo argv that names a surface the package cannot expose."""
    if row.get("suite") == "profile":
        return
    row_id = row.get("id", "<unknown>")
    package = row.get("package")
    if not isinstance(package, str):
        fail(f"{row_id} declares no cargo package")
    package_manifest = None
    package_document = None
    for candidate in ROOT.glob("crates/*/Cargo.toml"):
        try:
            document = tomllib.loads(candidate.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as error:
            fail(f"cargo manifest {candidate.relative_to(ROOT)} is unreadable: {error}")
        if document.get("package", {}).get("name") == package:
            package_manifest = candidate
            package_document = document
            break
    if package_manifest is None or package_document is None:
        fail(f"{row_id} names unknown cargo package {package!r}")

    features = row.get("features", [])
    if not isinstance(features, list) or any(not isinstance(value, str) for value in features):
        fail(f"{row_id} features are not a string list")
    if len(features) != len(set(features)):
        fail(f"{row_id} declares duplicate cargo features {features!r}")
    known_features = set(package_document.get("features", {}))
    unknown_features = sorted(set(features) - known_features)
    if unknown_features:
        fail(
            f"{row_id} names unavailable features {unknown_features!r} for package {package!r}"
        )

    package_root = package_manifest.parent
    target = row.get("test_target")
    if target == "lib":
        available = "lib" in package_document or (package_root / "src/lib.rs").is_file()
        expected = "library target"
    elif isinstance(target, str):
        tests = {
            entry.get("name")
            for entry in package_document.get("test", [])
            if isinstance(entry, dict)
        }
        tests.update(path.stem for path in (package_root / "tests").glob("*.rs"))
        available = target in tests
        expected = f"integration-test target {target!r}"
    else:
        available = False
        expected = "named cargo test target"
    if not available:
        fail(f"{row_id} requests unavailable {expected} from package {package!r}")


def load_rows(prefix):
    global REFUSAL_PREFIX
    REFUSAL_PREFIX = prefix
    if prefix not in EXACT_INVENTORIES:
        fail(f"unknown prefix {prefix!r}; ruled prefixes are {sorted(EXACT_INVENTORIES)}")
    try:
        document = json.loads(MANIFEST.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"manifest unreadable: {error}")
    rows = [row for row in document.get("mutations", []) if id_token(row.get("id", "")) == prefix]
    expected = EXACT_INVENTORIES[prefix]
    if [row.get("id") for row in rows] != expected:
        fail(f"row inventory is not exact: {[row.get('id') for row in rows]}")
    for manifest in ROOT.rglob("manifest.json"):
        if "mutations" not in manifest.parts:
            continue
        if manifest == MANIFEST:
            continue
        try:
            other = json.loads(manifest.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        ids = [row.get("id", "") for row in other.get("mutations", [])]
        if any(id_token(value) == prefix for value in ids):
            fail(f"prefix collision in {manifest.relative_to(ROOT)}")
    # The manifest also DECLARES a per-family row count. Nothing used to read it,
    # so it was duplicate data free to drift away from the rows it described - a
    # census that cannot be wrong because nobody asks it. Bind all three: the
    # declared count, the rows actually present, and the runner's exact inventory.
    # Checked last so the cross-manifest collision refusal keeps its precedence.
    declared = document.get("inventories")
    if not isinstance(declared, dict):
        fail("manifest declares no inventories block")
    if sorted(declared) != sorted(EXACT_INVENTORIES):
        fail(
            f"declared inventories {sorted(declared)} are not the ruled families "
            f"{sorted(EXACT_INVENTORIES)}"
        )
    entry = declared.get(prefix)
    if not isinstance(entry, dict) or "rows" not in entry:
        fail(f"manifest declares no row count for {prefix}")
    if entry["rows"] != len(expected):
        fail(
            f"declared {prefix} row count {entry['rows']} does not match the exact "
            f"inventory of {len(expected)}"
        )
    for row in rows:
        validate_cargo_row(row)
    return rows


def command(row):
    if row.get("suite") == "profile":
        # Relative on purpose: the runner executes with cwd set to the
        # disposable worktree, so this must be the mutated copy of the suite
        # and never the candidate tree's unmutated one.
        return [sys.executable, "remote-compute/test_profile.py"]
    base = ["cargo", "test", "-p", row["package"]]
    features = row.get("features", [])
    if features:
        base.extend(["--features", ",".join(features)])
    if row["test_target"] == "lib":
        base.extend(["--lib", row["test"]])
    else:
        base.extend(["--test", row["test_target"], row["test"]])
    base.append("--no-fail-fast")
    return base


def remove_worktree(path):
    if os.name == "nt" and path.exists():
        run(["cmd", "/c", "dir", "/a:l", str(path)])
    result = run(["git", "worktree", "remove", "--force", str(path)])
    if result.returncode != 0:
        fail(f"worktree removal failed for {path}: {result.stderr.strip()}")
    if path.exists():
        fail(f"worktree path still exists after removal: {path}")


def declared_targets():
    """Every file any ruled row edits - Rust or not."""
    try:
        document = json.loads(MANIFEST.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"manifest unreadable: {error}")
    targets = set()
    for row in document.get("mutations", []):
        for edit in row.get("edits") or [row]:
            if edit.get("file"):
                targets.add(edit["file"])
    return sorted(targets)


def residue_backstop(head):
    """Prove the candidate tree is byte-identical to HEAD for every edited path.

    The previous backstop globbed `crates/**/src/**/*.rs` for a `MUTATED:`
    marker. Both halves were too narrow. FS01-FS04 edit
    `remote-compute/*.json` and a fixture `manifest.json`, so nothing under
    `crates/**/src` - residue in a profile or fixture was invisible to the only
    check that ran. And a marker scan only finds residue that happens to carry
    the marker text; a reverted-but-not-restored byte carries nothing.

    Byte identity against HEAD is the check that cannot be fooled by shape: it
    covers every declared target whatever its language, and it fails on any
    difference rather than on a spelling we remembered to look for.
    """
    problems = []
    for relative in declared_targets():
        blob = git_blob(head, relative)
        if blob.returncode != 0:
            problems.append(f"{relative}: absent at HEAD")
            continue
        target = ROOT / relative
        if not target.exists():
            problems.append(f"{relative}: missing from the candidate tree")
            continue
        on_disk = target.read_bytes().replace(b"\r\n", b"\n")
        if on_disk != blob.stdout.replace(b"\r\n", b"\n"):
            problems.append(f"{relative}: differs from HEAD after the run")
    if problems:
        fail("mutation residue survived: " + "; ".join(problems))
    return len(declared_targets())


def coverage_refusal(executed):
    """Refuse any governed receipt that is not the exact ruled inventory.

    Extracted from `main` so it can be witnessed. Inline, it was unreachable
    from any test: `families` comes from `RULED_ORDER`, and an earlier guard
    already requires `RULED_ORDER` to cover `EXACT_INVENTORIES`, so in normal
    operation `executed` is always complete and the check never fires. It earns
    its place only against a source mutation that shortens the draw - which is
    precisely the defect this lane closed - and a guard whose deletion changes
    no observable behaviour is a guard nothing is holding.
    """
    expected = set(EXACT_INVENTORIES)
    observed = set(executed)
    missing = sorted(expected - observed)
    if missing:
        return (
            f"the governed no-argument run is missing ruled families {missing}; "
            f"expected {sorted(expected)}"
        )
    unknown = sorted(observed - expected)
    if unknown:
        return f"the governed no-argument run returned unknown families {unknown}"
    for prefix in sorted(expected):
        actual = executed[prefix]
        required = len(EXACT_INVENTORIES[prefix])
        if type(actual) is not int:
            return f"ruled family {prefix} returned invalid row count {actual!r}"
        if actual < required:
            return f"ruled family {prefix} returned short count {actual}; expected {required}"
        if actual > required:
            return f"ruled family {prefix} returned long count {actual}; expected {required}"
    return None


def restore_and_verify(row, path, head, edits):
    """A red in a disposable checkout counts only if restoration passes there."""
    for relative in sorted({edit["file"] for edit in edits}):
        original = git_blob(head, relative)
        if original.returncode != 0:
            fail(f"{row['id']} cannot restore its pinned original: {relative}")
        (path / relative).write_bytes(original.stdout)
    restored = run(command(row), cwd=path, env=worktree_env(path))
    if restored.returncode != 0:
        tail = (restored.stdout + restored.stderr)[-6144:]
        fail(f"{row['id']} original witness failed after restoration; red is not attributed:\n{tail}")


def run_family(prefix, head):
    return run_rows(prefix, head, load_rows(prefix))


def run_rows(prefix, head, rows):
    """Run explicitly supplied rows; only run_family grants whole-prefix scope."""
    for row in rows:
        path = Path(tempfile.gettempdir()) / f"grapt-{row['id'].lower()}-{os.getpid()}"
        if path.exists():
            fail(f"pre-existing mutation worktree path: {path}")
        added = run(["git", "worktree", "add", "--detach", str(path), head])
        if added.returncode != 0:
            fail(f"worktree add failed: {added.stderr.strip()}")
        try:
            edits = row.get("edits")
            if edits is None:
                edits = [{key: row[key] for key in ("file", "old", "new")}]
            if not edits:
                fail(f"{row['id']} declares no edits")
            for edit in edits:
                target = path / edit["file"]
                text = target.read_text(encoding="utf-8").replace("\r\n", "\n")
                count = text.count(edit["old"])
                if count != 1:
                    fail(f"{row['id']} anchor count is {count}, expected exactly one")
                if edit["old"] == edit["new"]:
                    fail(f"{row['id']} is a no-op")
                target.write_text(
                    text.replace(edit["old"], edit["new"], 1),
                    encoding="utf-8",
                    newline="\n",
                )
            result = run(command(row), cwd=path, env=worktree_env(path))
            evidence = result.stdout + result.stderr
            if result.returncode == 0:
                fail(f"{row['id']} survived")
            required_evidence = row.get("reds", [row.get("red")])
            if not required_evidence or any(value is None for value in required_evidence):
                fail(f"{row['id']} declares no named red evidence")
            missing = [value for value in required_evidence if value not in evidence]
            if missing:
                # Keep a bounded tail from the actual cargo invocation. A row
                # that went red for an unnamed reason is not attributable from
                # the missing substring alone, and the disposable worktree is
                # deliberately removed in `finally` immediately afterwards.
                # The tail is diagnostic only; it never satisfies the named
                # evidence predicate above.
                stdout_tail = result.stdout[-4096:].replace("\x00", "<NUL>")
                stderr_tail = result.stderr[-2048:].replace("\x00", "<NUL>")
                fail(
                    f"{row['id']} went red without named evidence {missing!r}; "
                    "bounded output tails follow:\n"
                    f"--- stdout ---\n{stdout_tail}\n"
                    f"--- stderr ---\n{stderr_tail}"
                )
            restore_and_verify(row, path, head, edits)
            print(f"{row['id']} RED then original GREEN: {', '.join(required_evidence)}")
        finally:
            remove_worktree(path)
    print(f"{len(rows)}/{len(rows)} {prefix} rows RED")
    return len(rows)


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--prefix":
        families = [sys.argv[2]]
    elif len(sys.argv) == 1:
        # No argument is the governed form. It draws EVERY ruled family.
        families = list(RULED_ORDER)
    else:
        fail("usage: run.py [--prefix IPR]")
    if sorted(RULED_ORDER) != sorted(EXACT_INVENTORIES):
        fail(
            f"ruled order {sorted(RULED_ORDER)} does not cover the ruled families "
            f"{sorted(EXACT_INVENTORIES)}"
        )
    head = run(["git", "rev-parse", "HEAD"])
    if head.returncode != 0:
        fail(head.stderr.strip())
    revision = head.stdout.strip()
    executed = {}
    for prefix in families:
        executed[prefix] = run_family(prefix, revision)
    # The residue backstop covers every declared target of every ruled family,
    # not only the families this invocation drew, so a hand-selected run still
    # proves it left nothing behind anywhere.
    checked = residue_backstop(revision)
    inventory = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--",
         ":(glob)crates/**/src/**/*.rs"],
        cwd=ROOT, capture_output=True, check=False,
    )
    if inventory.returncode != 0:
        fail("source inventory failed: " + inventory.stderr.decode("utf-8", errors="replace"))
    source_hits = [ROOT / name.decode("utf-8") for name in inventory.stdout.split(b"\0")
                   if name and (ROOT / name.decode("utf-8")).is_file()]
    if any("MUTATED:" in path.read_text(encoding="utf-8", errors="ignore") for path in source_hits):
        fail("a mutation marker survived in the candidate tree")
    summary = ", ".join(f"{prefix} {count}/{count} RED" for prefix, count in executed.items())
    print(f"residue backstop: {checked} declared target(s) byte-identical to {revision[:12]}")
    print(f"{sum(executed.values())} rows RED across {len(executed)} family/families: {summary}")
    if len(sys.argv) == 1:
        refusal = coverage_refusal(executed)
        if refusal:
            fail(refusal)


if __name__ == "__main__":
    main()
