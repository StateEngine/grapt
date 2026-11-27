"""Shared affected/full command policy for execution and landing verification."""
from pathlib import Path
import subprocess
import sys
import test_features
import slow_tests
import windows_gate
import nextest_gate
import windows_scope
from gate_policy import required_mode


from gate_scope import scoped_paths


def select(packages, paths, root):
    members = {p["id"]: p for p in packages}
    directories = {p["id"]: Path(p["manifest_path"]).parent.relative_to(root).as_posix()
                   for p in packages}
    # Workspace/build policy and shared fixtures can affect any member.
    shared = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "clippy.toml", "build.rs"}
    if any(p in shared or p.startswith((".cargo/", "third-party/", "test-support/"))
           or (p.startswith("fixtures/") and not p.startswith("fixtures/benchmarks/")) for p in paths):
        return sorted(p["name"] for p in packages)
    selected = {pid for pid, directory in directories.items()
                if any(p == directory + "/Cargo.toml" or p.startswith(directory + "/")
                       for p in paths)}
    # The overhaul requires touched crates and their direct consumers.
    names = {p["name"]: pid for pid, p in members.items()}
    consumers = {pid for pid, p in members.items()
                 if any(names.get(d.get("package") or d["name"]) in selected
                        for d in p["dependencies"])}
    result = {members[pid]["name"] for pid in selected | consumers}
    if any(p.startswith(("crates/grapt-api/", "crates/grapt-product/src/")) for p in paths):
        if "grapt-tui" in names:
            result.add("grapt-tui")
    return sorted(result)


def test_command(packages, workspace=False, *, nightly=False):
    selectors = ["--workspace"] if workspace else [arg for name in packages for arg in ("-p", name)]
    features = test_features.arguments(None if workspace else packages)
    # Product journeys serialize their real processes. A valid long holder
    # can outlast another test's bounded custody wait, so do not queue waiters.
    threads = 1 if workspace or "grapt-product" in packages else 4
    return ["cargo", "test", "--locked", *selectors, *features,
            "--no-fail-fast", "--", f"--test-threads={threads}",
            *([] if nightly else slow_tests.arguments(None if workspace else packages))]


def full_commands(packages, *, nightly=False, tui=False, cli=False):
    return nextest_gate.plan([
        ["cargo", "clippy", "--locked", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"],
        # Feature-enabled lint/tests cannot catch a test importing a gated helper.
        ["cargo", "check", "--locked", "--workspace", "--tests"],
        *windows_gate.commands(packages, workspace=nightly),
        # The product journeys launch the sibling CLI, which the workspace
        # test command compiles only as a side effect. Build it explicitly
        # with the CLI's acceptance features so copied binaries accept
        # loopback sign-in fixtures; a stale plain build left in the shared
        # target directory fails them with "sign-in URL is invalid". A
        # workspace without the CLI package (a fixture) plans no CLI build.
        *([["cargo", "build", "--locked", "-p", "grapt-cli", "--bin", "grapt",
            *test_features.arguments(["grapt-cli"])]] if cli else []),
        # The busy-seat journey drives the real TUI binary, which no workspace
        # test target builds; never inherit one left in the target directory.
        *([["cargo", "build", "--locked", "-p", "grapt-tui", "--bin", "grapt-tui"]] if tui else []),
        test_command(packages, workspace=True, nightly=nightly),
        *test_features.ordinary_commands(),
    ])


def affected_commands(packages, *, libraries=None):
    selectors = [arg for name in packages for arg in ("-p", name)]
    # Lint the same feature-gated test code that the affected test command runs.
    features = test_features.arguments(packages)
    # Product and TUI journeys launch the sibling CLI, which their package
    # selections do not build. Match the CLI's acceptance features so copied
    # binaries accept loopback sign-in fixtures. CLI selections already do so.
    prerequisites = ([["cargo", "build", "--locked", "-p", "grapt-cli", "--bin", "grapt",
                       *test_features.arguments(["grapt-cli"])]]
                     if {"grapt-product", "grapt-client"} & set(packages)
                     and "grapt-cli" not in packages else [])
    # The busy-seat journey drives the real TUI binary beside the product. A
    # grapt-tui selection builds only its test harness, so build it either way.
    if "grapt-product" in packages:
        prerequisites.append(["cargo", "build", "--locked", "-p", "grapt-tui", "--bin", "grapt-tui"])
    return nextest_gate.plan([
        ["cargo", "clippy", "--locked", *selectors, "--all-targets", *features,
         "--", "-D", "warnings"],
        *windows_gate.commands(packages),
        *prerequisites,
        test_command(packages),
        *test_features.ordinary_commands(packages),
    ], libraries=libraries)


def composition_commands(packages):
    """Check a disjoint rebase against the complete ordinary test graph."""
    result = [["cargo", "check", "--locked", "--workspace", "--tests"]]
    if packages:
        result.append(affected_commands(packages)[0])
        result.extend(windows_gate.commands(packages))
    return result


def law_commands():
    return [
        ["cargo", "test", "--locked", "-p", "grapt-types", "--test", "no_god_files",
         "--test", "direct_include_format", "--test", "crate_graph", "--no-fail-fast"],
        ["cargo", "test", "--locked", "-p", "grapt-store", "--test", "write_chokepoint",
         "--no-fail-fast"],
    ]


def helper_checks(paths, *, nightly=False):
    """The scheduler suite belongs to build-control changes and nightly coverage."""
    return nightly or any(path.startswith("scripts/build/") or path in {
        "scripts/land.ps1", "scripts/gate.sh", "scripts/build-cache.sh",
    } for path in paths)


def staging_select(packages, paths, root):
    """A staging precheck checks whole touched crates; CI owns the workspace."""
    return sorted(p['name'] for p in packages if any(
        path.startswith(Path(p['manifest_path']).parent.relative_to(root).as_posix() + '/')
        for path in paths))


def commands(mode, packages, paths, root, python=sys.executable, base=None, *, nightly=False, staging=False,
             batch=False, head=None):
    formatting = sorted(p["name"] for p in packages
                        if any(path.startswith(Path(p["manifest_path"]).parent.relative_to(root).as_posix()+"/")
                               for path in paths))
    selected = staging_select(packages, paths, root) if staging else select(packages, scoped_paths(paths, root, base), root)
    if mode == "composition":
        return composition_commands(selected)
    result = []
    if helper_checks(paths, nightly=nightly) or staging and any(path.startswith('scripts/') for path in paths):
        result.append([python, "-m", "unittest", "discover", "-s", "scripts/build", "-p", "test_*.py"])
    if staging and any(path.startswith('scripts/ci/') or path.startswith('.github/') for path in paths):
        result.append([python, "-m", "unittest", "discover", "-s", "scripts/ci", "-p", "test_*.py"])
    result.append([python, "scripts/release/licenses.py", "check"])
    if base:
        result.append([python, "scripts/build/ratchet_monotonic.py", base])
    if (root / "bench/test_runner.py").is_file() and (
            mode == "full" or any(path.startswith(("bench/", "fixtures/benchmarks/")) for path in paths)):
        result.append([python, "-m", "unittest", "discover", "-s", "bench", "-p", "test_*.py"])
    result.extend(["cargo", "fmt", "-p", name, "--check"] for name in formatting)
    if mode == "full":
        result.extend(full_commands(selected, nightly=nightly,
                                    tui=any(p["name"] == "grapt-tui" for p in packages),
                                    cli=any(p["name"] == "grapt-cli" for p in packages)))
        if nightly:
            # Minutes per large crate, so module cycles are a nightly law.
            result.append([python, "scripts/build/module_cycles.py", "check"])
    elif selected:
        libraries = {package['name'] for package in packages
                     if any(target.get('doctest', True) and set(target['kind']) & {'lib','rlib','dylib','staticlib','cdylib','proc-macro'}
                            for target in package.get('targets', []))}
        result.extend(affected_commands(selected, libraries=libraries))
    if mode == "affected":
        for law, owner in zip(law_commands(), ("grapt-types", "grapt-store")):
            if owner not in selected:
                result.append(law)
    if batch and not nightly and not windows_scope.required(paths, root, base, head):
        result = [command for command in result if command[:2] != ['cargo', 'xwin']]
    return result


ADVISORY_MAX_LINES = 700


def physical_line_count(source):
    """Count physical lines exactly as the no_god_files Rust law does."""
    lines = 0
    unterminated = False
    index = 0
    while index < len(source):
        byte = source[index]
        if byte == 0x0A:  # \n
            lines += 1
            index += 1
            unterminated = False
        elif byte == 0x0D:  # \r
            lines += 1
            index += 1
            if index < len(source) and source[index] == 0x0A:
                index += 1  # CRLF ends one line, not two
            unterminated = False
        else:
            index += 1
            unterminated = True
    return lines + (1 if unterminated else 0)


def _base_line_count(root, base, relpath):
    """The file's line count in the base commit, or None when unavailable.

    Zero is returned only when git proves the path absent from the base,
    which makes the file new in this change. A timeout, an OS error, or an
    unreadable tree or object leaves the baseline unknown, and that must
    never be reported as a new file.
    """
    try:
        listing = subprocess.run(['git', 'ls-tree', base, '--', relpath],
                                 cwd=root, timeout=10, capture_output=True)
    except (OSError, subprocess.SubprocessError):
        return None
    if listing.returncode != 0:
        return None
    if not listing.stdout.strip():
        return 0
    try:
        shown = subprocess.run(['git', 'show', f'{base}:{relpath}'],
                               cwd=root, timeout=10, capture_output=True)
    except (OSError, subprocess.SubprocessError):
        return None
    if shown.returncode != 0:
        return None
    return physical_line_count(shown.stdout)


def god_file_advisories(paths, root, base):
    """Advisory lines for changed crates .rs files that end over ADVISORY_MAX_LINES.

    The gate prints these before its commands; they never fail a gate. A git
    timeout or a missing base silences the whole check, an unreadable or
    deleted file silences that file only, and a file git proves absent from
    the base is new, so its whole length belongs to this change. Only the
    given changed paths are read; the workspace is never walked.
    """
    try:
        subprocess.check_output(['git', 'rev-parse', '--verify', f'{base}^{{commit}}'],
                                cwd=root, timeout=10, stderr=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError):
        return []
    advisories = []
    for relpath in sorted(p for p in paths
                          if p.startswith('crates/') and p.endswith('.rs')):
        try:
            head = physical_line_count(Path(root, relpath).read_bytes())
        except OSError:
            continue
        if head <= ADVISORY_MAX_LINES:
            continue
        before = _base_line_count(root, base, relpath)
        if before is None:
            continue
        delta = head - before
        if delta > 0:
            note = f'(+{delta} in this change)'
        elif delta < 0:
            note = f'({delta} in this change)'
        else:
            note = '(line count unchanged in this change)'
        advisories.append(
            f'{relpath}: {head} lines {note}, over {ADVISORY_MAX_LINES}: plan a split')
    return advisories
