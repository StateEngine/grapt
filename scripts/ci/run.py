"""Translate the existing gate policy into hosted build and archived-run jobs."""


# The gate advisory on the planned shard balance; a different role from the
# generator's SPLIT_VOLUME_FRACTION, which decides the file's shape.
BALANCE_ADVISORY_FRACTION = 0.10
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/build"))
import flaky_quarantine
import gate_plan
import gate_scratch
import nextest_gate
import slow_tests
import test_features
import windows_gate
import hosted_coverage as allocation
import junit_report


def run(command, *, capture=False):
    print("+ " + subprocess.list2cmdline(command), flush=True)
    start = time.monotonic()
    try:
        return subprocess.run(command, cwd=ROOT, check=True, text=True, encoding="utf-8",
                              stdout=subprocess.PIPE if capture else None).stdout
    finally:
        print(f"Command seconds: {time.monotonic() - start:.3f}", flush=True)


def selectors(packages):
    return ["--workspace"] if packages == ["*"] else [arg for p in packages for arg in ("-p", p)]


def run_helpers():
    import gate
    directories = ["scripts/ci"]
    # The build tooling's own suite qualifies changes to that tooling, which
    # always select the full gate; an affected landing does not rerun it.
    if (os.environ.get("GITHUB_EVENT_NAME") != "pull_request_target"
            and os.environ.get("CI_NIGHTLY") != "true"
            and os.environ.get("CI_GATE_MODE", "full") == "full"):
        directories.append("scripts/build")
    # Product-facing script suites are cheap and run in every mode.
    directories += ["scripts/daily-use", "scripts/stateengine-native"]
    deadline = time.monotonic() + 1800
    for directory in directories:
        gate.execute([sys.executable, "-m", "unittest", "discover", "-s", directory,
                      "-p", "test_*.py", "-v"], sys.stdout, deadline, cwd=ROOT)
    # The release packager's refusal suite is written for pytest.
    gate.execute([sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider", "scripts/release"],
                 sys.stdout, deadline, cwd=ROOT)
    # Enforce monotonic shrinking of structural ratchet ledgers against the CI base commit.
    base = os.environ.get("CI_BASE_SHA", "").strip()
    if not base:
        probe = subprocess.run(["git", "merge-base", "HEAD", "origin/main"],
                               cwd=ROOT, capture_output=True, text=True, timeout=30)
        if probe.returncode != 0 or not probe.stdout.strip():
            raise RuntimeError(
                f"cannot resolve base commit for ratchet_monotonic.py: {probe.stderr.strip()}"
            )
        base = probe.stdout.strip()
    gate.execute([sys.executable, "scripts/build/ratchet_monotonic.py", base],
                 sys.stdout, deadline, cwd=ROOT)


def validate_shard_selectors(common, features, rows, plan):
    """Prove each emitted shard selector at the nextest engine itself.

    The listing universe includes withheld and ignored rows (--run-ignored
    all), so a selector that leaks any of them fails the archive job instead
    of surfacing later as a consumer mismatch.
    """
    document = {(row["binary"], row["name"]): row["route"] for row in rows}
    hosted = {key for key, route in document.items() if route == "hosted"}
    selected = {}
    for shard in ("1", "2"):
        listing = run(["cargo", "nextest", "list", *common, *features,
                       "--user-config-file", "none", "--run-ignored", "all",
                       "-E", plan["shards"][shard]["expression"]], capture=True)
        cases = set()
        for line in listing.splitlines():
            binary, separated, name = line.strip().partition(" ")
            if not separated or not name:
                raise ValueError("nextest list emitted an unparseable row: " + line)
            key = (binary, name)
            if key not in document:
                raise ValueError("A shard selector selected an unknown case: " + line)
            if document[key] != "hosted":
                raise ValueError("A shard selector leaked a withheld case: " + line)
            cases.add(key)
        expected = {tuple(pair) for pair in plan["shards"][shard]["expected"]}
        if cases != expected:
            raise ValueError(f"Shard {shard} selector does not match its planned identity")
        selected[shard] = cases
    if selected["1"] & selected["2"]:
        raise ValueError("Shard selectors are not disjoint")
    if selected["1"] | selected["2"] != hosted:
        raise ValueError("Shard selectors do not cover the hosted cases exactly once")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["helpers", "fmt", "clippy", "windows-compile", "archive", "test", "doctest"])
    parser.add_argument("--platform", choices=["linux", "windows", "macos"], default="linux")
    parser.add_argument("--shard", type=int, choices=[1, 2], default=1)
    args = parser.parse_args()
    if args.action == "helpers":
        run_helpers()
        return
    packages = json.loads(os.environ["CI_PACKAGES"])
    if not isinstance(packages, list) or not packages or len(packages) > 256 or any(
            not isinstance(p, str) or p != "*" and not p.replace("-", "").isalnum() for p in packages):
        raise ValueError("Invalid hosted package scope")
    if "*" in packages and packages != ["*"]:
        raise ValueError("Workspace scope cannot be mixed with packages")
    workspace = packages == ["*"]
    selected = None if workspace else packages
    features = test_features.arguments(selected)
    scope = selectors(packages)
    target = ["--target", windows_gate.TARGET] if args.platform == "windows" else []
    common = ["--locked", *scope, *target]
    nightly = os.environ.get("CI_NIGHTLY") == "true"
    results = ROOT / "ci-results"
    results.mkdir(exist_ok=True)
    archive = ROOT / "tests.tar.zst"
    if args.action == "fmt":
        metadata = json.loads(run(["cargo", "metadata", "--no-deps", "--locked", "--format-version=1"], capture=True))
        for p in metadata["packages"]:
            if workspace or p["name"] in packages:
                run(["cargo", "fmt", "-p", p["name"], "--check"])
    elif args.action == "clippy":
        if args.platform == "linux" and workspace:
            run(["cargo", "clippy", *common, "--all-targets", "--all-features", "--", "-D", "warnings"])
            run(["cargo", "check", *common, "--tests"])
        else:
            for enabled in ([features, []] if features else [[]]):
                run(["cargo", "clippy", *common, *enabled, "--all-targets", "--", "-D", "warnings"])
    elif args.action == "windows-compile":
        if args.platform != "linux":
            raise ValueError("Windows cross-compilation requires the Linux host")
        for command in windows_gate.commands(packages, workspace=workspace):
            run(command)
    elif args.action == "archive":
        if workspace or set(packages) & set(slow_tests.TESTS):
            slow_tests.validate(ROOT)
        assets = {}
        if workspace or {"grapt-product", "grapt-client"} & set(packages):
            assets["grapt-cli"] = "grapt"
        if workspace or "grapt-client" in packages:
            assets["grapt-product"] = "grapt-server"
        # Selected packages' integration targets already ask Cargo for these
        # ordinary binaries via CARGO_BIN_EXE. Build only out-of-scope siblings;
        # the single nextest build supplies both harnesses and selected binaries.
        extra = set() if workspace else set(assets) - set(packages)
        if extra:
            asset_scope = sorted(set(packages) | extra)
            run(["cargo", "build", "--locked", *selectors(asset_scope), *target,
                 *test_features.arguments(asset_scope),
                 *[arg for owner in sorted(extra) for arg in ("--bin", assets[owner])]])
        run(["cargo", "nextest", "archive", *common, *features, "--profile", "gate",
             "--user-config-file", "none", "--archive-file", str(archive)])
        directory = ROOT / "target"
        if args.platform == "windows":
            directory /= windows_gate.TARGET
        for name in assets.values():
            binary = name + (".exe" if args.platform == "windows" else "")
            if not (directory / "debug" / binary).is_file():
                raise ValueError("Archive build did not supply its required sibling binary: " + binary)
        document = json.loads(run(["cargo", "nextest", "list", *common, *features,
                                   "--message-format=json", "--user-config-file", "none"], capture=True))
        metadata = json.loads(run(["cargo", "metadata", "--no-deps", "--locked", "--format-version=1"], capture=True))
        rows = allocation.allocate(document, metadata["packages"], args.platform, nightly)
        allocation.write(results / "test-allocation.json", rows)
        # Compiled-in fixture paths are not relocatable; require the same paths
        # on the archive producer and each consumer, rather than mask missing bins.
        (results / "archive-paths.json").write_text(json.dumps({"workspace": str(ROOT),
            "target": str(ROOT / "target")}) + "\n", encoding="utf-8")
        # Time-based sharding: read the checked-in timing file, decide the
        # shard mode once, and prove the emitted selectors before publishing.
        # The plan is a function of the commit only; nothing is fetched.
        try:
            weights, source = allocation.timing_input(
                ROOT / "scripts" / "ci" / "test-times.json")
            provenance = {"source": source, "reason": "ok"}
        except ValueError as error:
            weights, source = None, None
            provenance = {"source": None, "reason": "timing-input-unavailable",
                          "detail": str(error)}
        plan = allocation.shard_plan(rows, weights)
        if plan["mode"] == "time" and args.platform == "windows":
            # Windows bounds the assembled command line; decide the mode once
            # here, before any nextest invocation, over the full listing and
            # execution commands. The execution shape still carries
            # --retries, the longest form the consumer may assemble.
            policy = nextest_gate.plan([gate_plan.test_command(
                packages, workspace=workspace, nightly=nightly)])[0]
            options = policy[policy.index("--profile"):]
            if "-E" in options:
                options = options[:options.index("-E")]
            listing_argv = ["cargo", "nextest", "list", *common, *features,
                            "--user-config-file", "none", "--run-ignored", "all"]
            execution_argv = ["cargo", "nextest", "run", "--archive-file", str(archive),
                              "--extract-to", str(ROOT), "--extract-overwrite",
                              "--workspace-remap", str(ROOT), "--no-fail-fast",
                              *options]
            if any(
                    allocation.windows_command_units(
                        listing_argv + ["-E", plan["shards"][shard]["expression"]])
                    > allocation.WINDOWS_COMMAND_LIMIT
                    or allocation.windows_command_units(
                        execution_argv + ["-E", plan["shards"][shard]["expression"]])
                    > allocation.WINDOWS_COMMAND_LIMIT
                    for shard in ("1", "2")):
                # Both consumers keep the shared routing expression and the
                # literal slice partition instead of an oversized selector.
                plan = allocation.shard_plan(rows, None)
                provenance["fallback_reason"] = "windows-command-budget"
        elif plan["mode"] == "fallback" and weights is not None:
            # The file loaded but no time allocation is possible (for
            # example a single aggregate-only binary); the literal slice
            # fallback is the only honest partition.
            provenance["fallback_reason"] = "no-time-allocation"
        plan["provenance"] = provenance
        if plan["mode"] == "time":
            validate_shard_selectors(common, features, rows, plan)
            totals = plan["totals"]
            peak = max(totals["1"], totals["2"])
            if peak and abs(totals["1"] - totals["2"]) / peak > BALANCE_ADVISORY_FRACTION:
                print(f"Shard balance advisory: {totals['1']:.1f}s vs {totals['2']:.1f}s "
                      f"({abs(totals['1'] - totals['2']) / peak:.0%} of the larger shard); "
                      "the split still ships", flush=True)
        (results / "shard-plan.json").write_text(json.dumps(plan, indent=2) + "\n",
                                                 encoding="utf-8")
    elif args.action == "test":
        paths = json.loads((results / "archive-paths.json").read_text(encoding="utf-8"))
        if Path(paths["workspace"]).resolve() != ROOT or Path(paths["target"]).resolve() != ROOT / "target":
            raise ValueError("Compiled-in fixture paths differ between archive jobs")
        rows = json.loads((results / "test-allocation.json").read_text(encoding="utf-8"))
        allocation.write(results / "test-allocation.json", rows)
        try:
            plan = json.loads((results / "shard-plan.json").read_text(encoding="utf-8"))
        except (OSError, ValueError) as error:
            raise ValueError("The published shard plan is missing or corrupted; "
                             "refuse to guess the shard mode") from error
        if (plan.get("schema") != 1 or plan.get("mode") not in ("time", "fallback")
                or plan.get("total") not in (0, 1, 2)
                or (plan["mode"] == "time" and (plan["total"] != 2 or not plan.get("shards")))
                or (plan["mode"] == "fallback" and not plan.get("expression"))):
            raise ValueError("The published shard plan has an unsupported shape")
        # The plan must agree with the allocation this checkout produced; a
        # stale or corrupted plan must refuse, never shrink coverage into a
        # successful empty shard.
        hosted = {(row["binary"], row["name"]) for row in rows
                  if row["route"] == "hosted"}
        if plan["mode"] == "fallback":
            if (plan.get("shards") or plan["total"] != min(2, len(hosted))
                    or plan["expression"] != allocation.expression(rows)):
                raise ValueError("The published fallback plan disagrees with the allocation")
        else:
            # The plan is a partition: every hosted identity in exactly one
            # shard. A flattened union would let both consumers repeat work.
            if set(plan["shards"]) != {"1", "2"}:
                raise ValueError("The published shard plan has an unsupported shape")
            per_shard = {}
            for shard in ("1", "2"):
                entries = plan["shards"][shard].get("expected")
                if not isinstance(entries, list) or not entries:
                    raise ValueError("The published shard plan has an unsupported shape")
                cases = set()
                for pair in entries:
                    if not isinstance(pair, list) or len(pair) != 2:
                        raise ValueError("The published shard plan has an unsupported shape")
                    identity = tuple(pair)
                    if identity in cases:
                        raise ValueError("The published shard plan repeats an identity")
                    cases.add(identity)
                per_shard[shard] = cases
            if per_shard["1"] & per_shard["2"]:
                raise ValueError("The published shard plans overlap")
            if per_shard["1"] | per_shard["2"] != hosted:
                raise ValueError("The published shard identities disagree with the allocation")
        total = plan["total"]
        if args.shard > total:
            print("No hosted tests assigned to this shard; retained work is named in test-allocation.json")
            return
        try:
            with gate_scratch.scratch():
                # trybuild runs real offline compiler probes inside archived
                # tests; the archive has executables, not the consumer's
                # Cargo registry sources.
                run(["cargo", "fetch", "--locked"])
                policy = nextest_gate.plan([gate_plan.test_command(packages, workspace=workspace, nightly=nightly)])[0]
                options = policy[policy.index("--profile"):]
                if "-E" in options:
                    options = options[:options.index("-E")]
                # As the box gate does: validate the shipped flaky
                # quarantine, then let its profile overrides retry those
                # entries (the flag would outrank them).
                config = ROOT / ".config/nextest.toml"
                shipped = config.read_text(encoding="utf-8") if config.is_file() and not config.is_symlink() else ""
                issues = flaky_quarantine.problems(shipped) if shipped else []
                if issues:
                    raise ValueError("Flaky quarantine policy failed: " + "; ".join(issues))
                if shipped and flaky_quarantine.config_block(shipped) and "--retries" in options:
                    index = options.index("--retries")
                    options = options[:index] + options[index + 2:]
                if workspace or "grapt-client" in packages:
                    directory = ROOT / "target"
                    if args.platform == "windows":
                        directory /= windows_gate.TARGET
                    name = "grapt-server.exe" if args.platform == "windows" else "grapt-server"
                    os.environ["GRAPT_TUI_DOGFOOD_PRODUCT"] = str(directory / "debug" / name)
                if plan["mode"] == "time":
                    shard_plan = plan["shards"][str(args.shard)]
                    run(["cargo", "nextest", "run", "--archive-file", str(archive), "--extract-to", str(ROOT),
                         "--extract-overwrite", "--workspace-remap", str(ROOT),
                         "--no-fail-fast", *options, "-E", shard_plan["expression"]])
                    # Fail closed on any drift between the proven selector and
                    # what the archived build actually executed.
                    executed = junit_report.executed_identities(
                        ROOT / "target" / "nextest" / "gate" / "junit.xml")
                    expected = {tuple(pair) for pair in shard_plan["expected"]}
                    if executed != expected:
                        missing = sorted(f"{b}::{n}" for b, n in sorted(expected - executed))
                        extra = sorted(f"{b}::{n}" for b, n in sorted(executed - expected))
                        raise ValueError("Executed shard identities differ from the published plan: "
                                         f"missing={missing} extra={extra}")
                else:
                    run(["cargo", "nextest", "run", "--archive-file", str(archive), "--extract-to", str(ROOT),
                         "--extract-overwrite", "--workspace-remap", str(ROOT), "--partition", f"slice:{args.shard}/{total}",
                         "--no-fail-fast", *options, "-E", plan["expression"]])
                # The suite process checks its own headless base before it
                # may present the shard as passed.
                if gate_scratch.judge_headless_residue(gate_scratch.active_headless_base,
                                                       require_clean=True) != 0:
                    raise SystemExit(1)
        except gate_scratch.HeadlessBaseRefused as error:
            print(error, file=sys.stderr)
            raise SystemExit(75)
    else:
        commands = test_features.ordinary_commands(selected)
        rows = allocation.ordinary(args.platform) if commands else []
        allocation.write(results / "ordinary-allocation.json", rows)
        run(["cargo", "test", *common, *features, "--doc", "--no-fail-fast"])
        # These ordinary binaries bootstrap the same protected Product process
        # as the archived Windows cases already allocated to the native gate.
        if args.platform != "windows":
            for command in commands:
                run(command[:3] + target + command[3:])


if __name__ == "__main__":
    main()
