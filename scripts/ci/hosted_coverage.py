"""Allocate every archived test explicitly; hosted Windows is a partial gate.

Shard timing comes from the checked-in test-times.json (a function of the
commit only): no network at gate time, and no blocking I/O on the
critical path. A missing or malformed file is a planning failure that
keeps the literal slice fallback; a published plan is never guessed by a
consumer.
"""
import json
import math
import statistics
import subprocess
from pathlib import Path
import sys

from junit_report import MAX_BYTES

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/build"))
import slow_tests

MXC = {"tests::native_child_preserves_exit_and_refuses_protected_writes",
       "tests::native_network_off_refuses_the_same_reachable_direct_ip"}
# These public-tool cases actually completed on both hosted Windows tokens.
CODING = {"dogfood::tools::coding::tests::windows_paths::ordinary_dos_path_matches_canonical_workspace",
          "dogfood::tools::coding::tests::windows_paths::equivalent_drive_and_unc_prefixes_keep_components",
          "dogfood::tools::coding::tests::windows_paths::windows_prefix_aliases_do_not_expand_authority",
          "dogfood::tools::coding::tests::profile_protection::profile_files_refuse_reads_and_writes_through_public_tool_inputs"}


# Exact library cases qualified with the trusted protection and fixture imports.
CREDENTIALS = {
    ('grapt-connections', 'grapt-connections'): {
        'chatgpt::tests::a_migrated_sign_in_updates_the_managed_account_and_retains_user_configuration',
        'chatgpt::tests::a_new_sign_in_on_the_same_account_replaces_only_the_credential_and_listing',
        'store::tests::chatgpt_and_openai_have_independent_ids_and_storage',
        'store::tests::corrupt_unreadable_and_foreign_storage_never_become_absent',
        'store::tests::interrupted_publication_retains_complete_generation_and_exclusive_lock',
        'store::tests::key_install_distinguishes_visible_commits_and_rejects_oauth_before_writes',
        'store::tests::key_install_recovers_each_side_of_the_metadata_commit_without_resurrection',
        'store::tests::migration_preserves_legacy_ids_files_and_selection_and_is_idempotent',
        'store::tests::removal_recovers_both_sides_of_commit_without_restoring_slots_or_staging',
    },
    ('grapt-login', 'grapt-login'): {
        'chatgpt::migration::tests::a_foreign_writable_parent_cannot_cause_a_partially_migrated_profile',
        'chatgpt::tests::protection::save_and_load_keep_both_token_files_protected_on_the_atomic_path',
        'chatgpt::tests::protection::startup_migrates_legacy_tokens_before_any_provider_request',
        'profile_security::private_read::tests::private_snapshot_refuses_exposure_links_and_overflow',
        'profile_security::request_budget::tests::a_contending_caller_cannot_restart_the_budget_after_admission',
        'profile_security::tests::newly_created_private_record_gets_strict_user_authority',
        'profile_security::tests::only_real_directories_can_be_protected',
        'profile_security::tests::plaintext_input_remains_bounded_and_an_envelope_never_falls_back',
        'profile_security::tests::protected_storage_round_trips_binary_input_with_independent_plaintext_bound',
        'profile_security::windows_protocol::tests::only_the_complete_matching_reply_can_supply_a_protection_result',
        'vault::tests::deletion_clears_owned_staging_and_preserves_foreign_links',
        'vault::tests::private_publication_recovers_staging_and_refuses_corruption_or_contention',
        'vault::tests::stored_credentials_keep_connection_destination_and_payload_purpose',
    },
    ('grapt-platform', 'grapt-platform'): {
        'credential::connection_use::environment_tests::retained_environment_grant_ends_at_managed_disconnect_without_reading_the_value',
        'credential::renewal::tests::vaulted_credentials_keep_their_binding_on_initial_and_renewed_capabilities',
        'profile_configuration::command_trust_tests::explicit_connection_changes_preserve_frozen_configuration_and_reject_unrelated_edits',
    },
}


def protection_consumers(packages):
    consumers = {"grapt-login"}
    while True:
        revised = consumers | {p["name"] for p in packages
                               if any(d["name"] in consumers for d in p["dependencies"])}
        if revised == consumers:
            return consumers
        consumers = revised


def allocate(document, packages, platform, nightly):
    consumers = protection_consumers(packages)
    rows = []
    for binary, suite in document["rust-suites"].items():
        package = suite["package-name"]
        for name, case in suite["testcases"].items():
            route, reason = "hosted", "ordinary test"
            if case["ignored"]:
                route, reason = "ignored", "Retain the test's original execution and input requirements"
            elif platform == "windows" and package == "grapt-sandbox" and name in MXC:
                route, reason = "vm", "Hosted Windows lacks the native MXC security environment API"
            elif platform == "windows" and package == "grapt-client" and binary.endswith("::dogfood_journey"):
                route, reason = "vm", "Real TUI journeys bootstrap the credential-dependent Product process"
            elif platform == "windows" and package in consumers and not (
                    (package == "grapt-product" and name in CODING) or
                    name in CREDENTIALS.get((package, binary), ())):
                route, reason = "vm", "Credential helper or dependent runtime coverage remains native until cold-start support is established"
            elif not nightly and name in slow_tests.TESTS.get(package, {}):
                route, reason = "nightly", "Exact slow-test tag from the landing gate"
            rows.append({"binary": binary, "package": package, "name": name, "route": route, "reason": reason})
    if not rows:
        raise ValueError("An archive must list at least one test")
    return rows


def ordinary(platform):
    names = ("ordinary_binary_ignores_the_fixture_crash_selector",
             "lifecycle::witnesses::ordinary_binary_ignores_the_fixture_lifecycle_selectors")
    return [{"binary": "grapt-product::handover_journey", "package": "grapt-product",
             "name": name, "route": "vm" if platform == "windows" else "hosted",
             "reason": "Ordinary handover guards bootstrap the credential-dependent Product process"
             if platform == "windows" else "Ordinary binary must ignore fixture selectors"}
            for name in names]


def expression(rows):
    # Package routing is conservative; every withheld case is also named in the
    # artifact. Exact binary and case predicates prevent substring exclusions.
    by_binary = {}
    for row in rows:
        by_binary.setdefault(row["binary"], []).append(row)
    terms = []
    for binary, suite in by_binary.items():
        included = [row["name"] for row in suite if row["route"] == "hosted"]
        excluded = [row["name"] for row in suite if row["route"] != "hosted"]
        if not excluded:
            continue
        if not included:
            terms.append(f"binary_id(={binary})")
        else:
            denied = "(" + " | ".join(f"test(={name})" for name in excluded) + ")"
            allowed = "not (" + " | ".join(f"test(={name})" for name in included) + ")"
            # In a mostly withheld binary, negate the few exact hosted cases
            # rather than put thousands of excluded names in a Windows argv.
            terms.append(f"(binary_id(={binary}) & " + min((denied, allowed), key=len) + ")")
    return "not (" + " | ".join(terms) + ")" if terms else "all()"


# CreateProcessW's 32767-unit command line includes the terminating null,
# so the quotable content is one unit shorter.
WINDOWS_COMMAND_LIMIT = 32766


def windows_command_units(argv):
    """UTF-16 code units of the command line Windows would assemble.

    CreateProcess bounds the assembled line, quoted and single-spaced, in
    UTF-16 units, so the budget is measured on exactly that string.
    """
    return len(subprocess.list2cmdline(argv).encode("utf-16-le")) // 2


def write(path, rows):
    path.write_text(json.dumps(rows, indent=2) + "\n", encoding="utf-8")
    counts = {route: sum(row["route"] == route for row in rows)
              for route in ("hosted", "vm", "nightly", "ignored")}
    print("Test allocation: " + json.dumps(counts), flush=True)
    return counts


TIMING_SCHEMA = 1


def _duration(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError("Timing duration is not a number")
    try:
        seconds = float(value)
    except OverflowError:
        raise ValueError("Timing duration is not finite nonnegative")
    if not math.isfinite(seconds) or seconds < 0:
        raise ValueError("Timing duration is not finite nonnegative")
    return seconds


def timing_input(path, bound=None):
    """(weights, source) from the checked-in timing file.

    Bounded and fail-closed: a missing, oversized, malformed or
    schema-mismatched file raises ValueError so the caller keeps the
    literal slice fallback. The weights carry per-binary totals for every
    binary and per-test maps only for the mapped ones; the file stores no
    fact about a test it did not observe.
    """
    if bound is None:
        bound = MAX_BYTES
    try:
        with open(path, "rb") as stream:
            raw = stream.read(bound + 1)
    except OSError:
        raise ValueError("Timing file is missing")
    if len(raw) > bound:
        raise ValueError("Timing file exceeds its bound")
    try:
        document = json.loads(raw)
    except ValueError:
        raise ValueError("Timing file does not parse")
    if (not isinstance(document, dict)
            or set(document) != {"schema", "source", "binaries"}
            or document["schema"] != TIMING_SCHEMA
            or not isinstance(document["source"], str) or not document["source"]
            or not isinstance(document["binaries"], dict) or not document["binaries"]):
        raise ValueError("Timing file has an unsupported shape")
    totals, tests = {}, {}
    for binary, entry in document["binaries"].items():
        if (not isinstance(binary, str) or not binary
                or not isinstance(entry, dict)
                or set(entry) not in ({"total"}, {"total", "tests"})):
            raise ValueError("Timing file has an unsupported binary entry")
        totals[binary] = _duration(entry["total"])
        if "tests" in entry:
            mapping = entry["tests"]
            if not isinstance(mapping, dict) or not mapping:
                raise ValueError("Timing file has an unsupported test map")
            for name in mapping:
                if not isinstance(name, str) or not name:
                    raise ValueError("Timing file has an unsupported test name")
            tests[binary] = {name: _duration(seconds) for name, seconds in mapping.items()}
    return {"totals": totals, "tests": tests}, document["source"]


def allocate_by_time(hosted, weights):
    """Deterministic LPT shard assignment over per-binary totals.

    Every binary's weight is its checked-in total; an unknown binary takes
    the median of the known totals, floored at 1.0 s. A single binary is
    split at test level only when the file carries its per-test map, where
    an unknown test takes the median of the binary's known test times,
    floored at 1.0 s. A single aggregate-only or unknown binary cannot be
    split, and any allocation leaving a shard empty returns None so the
    caller keeps the literal slice partition.
    """
    if len(hosted) < 2 or not weights:
        return None
    totals, tests = weights["totals"], weights["tests"]
    by_binary = {}
    for row in hosted:
        by_binary.setdefault(row["binary"], []).append(row)
    known = [totals[binary] for binary in by_binary if binary in totals]
    if not known:
        return None
    default_total = max(1.0, statistics.median(known))
    load = {1: 0.0, 2: 0.0}
    assignment = {}
    split = {"binary": None, "reason": None}
    if len(by_binary) == 1:
        binary = next(iter(by_binary))
        test_map = tests.get(binary)
        if not test_map:
            # An aggregate-only or unknown single binary carries no per-test
            # fact; the shared literal slice fallback can still partition
            # its tests, this plan cannot.
            split["reason"] = "single-binary-without-test-map"
            return None
        median = max(1.0, statistics.median(test_map.values()))
        order = sorted(by_binary[binary],
                       key=lambda row: (-test_map.get(row["name"], median), row["name"]))
        for row in order:
            shard = 1 if load[1] <= load[2] else 2
            assignment[(row["binary"], row["name"])] = shard
            load[shard] += test_map.get(row["name"], median)
        split = {"binary": binary, "source": "file-test-map"}
        defaults_applied = sum(1 for row in by_binary[binary] if row["name"] not in test_map)
        binary_weights = {binary: {"weight": totals[binary], "source": "file"}}
    else:
        binary_weight = {binary: totals.get(binary, default_total) for binary in by_binary}
        order = sorted(by_binary, key=lambda binary: (-binary_weight[binary], binary))
        binary_shard = {}
        for binary in order:
            shard = 1 if load[1] <= load[2] else 2
            binary_shard[binary] = shard
            load[shard] += binary_weight[binary]
        for row in hosted:
            assignment[(row["binary"], row["name"])] = binary_shard[row["binary"]]
        split = {"binary": None, "reason": "multi-binary"}
        defaults_applied = sum(1 for binary in by_binary if binary not in totals)
        binary_weights = {binary: {"weight": binary_weight[binary],
                                   "source": "file" if binary in totals else "default"}
                          for binary in by_binary}
    if 1 not in assignment.values() or 2 not in assignment.values():
        return None
    unknown_tests = {}
    for binary, rows in by_binary.items():
        test_map = tests.get(binary)
        if test_map is None:
            continue
        missing = sum(1 for row in rows if row["name"] not in test_map)
        if missing:
            unknown_tests[binary] = missing
    return {"assignment": assignment, "totals": load,
            "allocation": {"binary_weights": binary_weights, "split": split,
                           "unknown_binaries": sorted(binary for binary in by_binary
                                                      if binary not in totals),
                           "unknown_tests": unknown_tests,
                           "aggregate_only": sorted(binary for binary in by_binary
                                                    if binary in totals and binary not in tests),
                           "defaults_applied": defaults_applied}}


def shard_expression(rows, assignment, shard):
    """One combined routing-and-shard expression for a single shard.

    Every term keeps the shipped shorter-of-allowed/denied construction;
    the archive producer still checks the full assembled command against
    the Windows budget and selects the common fallback when it overflows.
    """
    by_binary = {}
    for row in rows:
        by_binary.setdefault(row["binary"], []).append(row)
    terms = []
    for binary, suite in by_binary.items():
        included = sorted(row["name"] for row in suite
                          if row["route"] == "hosted"
                          and assignment.get((binary, row["name"])) == shard)
        if not included:
            continue
        withheld = sorted(row["name"] for row in suite
                          if row["route"] != "hosted"
                          or assignment.get((binary, row["name"])) != shard)
        if not withheld:
            terms.append(f"binary_id(={binary})")
        else:
            allowed = "(" + " | ".join(f"test(={name})" for name in included) + ")"
            denied = "not (" + " | ".join(f"test(={name})" for name in withheld) + ")"
            terms.append(f"binary_id(={binary}) & " + min((allowed, denied), key=len))
    if not terms:
        raise ValueError(f"Shard {shard} selects no test binary")
    return " | ".join(terms)


def shard_plan(rows, weights):
    """The persisted shard decision both consumers must obey.

    Time mode needs usable file weights and a nonempty shard for each
    consumer; anything else keeps the literal slice partition behind the
    shared routing expression, byte-identical to the previous behavior.
    """
    counts = {route: sum(row["route"] == route for row in rows)
              for route in ("hosted", "vm", "nightly", "ignored")}
    hosted = [row for row in rows if row["route"] == "hosted"]
    plan = {"schema": 1, "mode": "fallback", "total": min(2, counts["hosted"]),
            "expression": expression(rows), "shards": {}}
    allocated = allocate_by_time(hosted, weights)
    if allocated is None:
        return plan
    assignment, totals = allocated["assignment"], allocated["totals"]
    shards = {}
    for shard in (1, 2):
        cases = sorted(key for key, owner in assignment.items() if owner == shard)
        shards[str(shard)] = {
            "expression": shard_expression(rows, assignment, shard),
            "expected": [[binary, name] for binary, name in cases],
        }
    plan.update(mode="time", total=2,
                totals={str(shard): totals[shard] for shard in (1, 2)},
                allocation=allocated["allocation"],
                shards=shards)
    return plan
