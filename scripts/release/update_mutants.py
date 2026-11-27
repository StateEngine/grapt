#!/usr/bin/env python3
"""UPR unit counterexamples; native package/process witnesses are separate."""
import argparse
from dataclasses import dataclass
import json
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/build"))
from gate import execute

CLI = "crates/grapt-cli/src/installation/"
GUARD = "crates/grapt-service/src/installation.rs"


@dataclass(frozen=True)
class Edit:
    path: str
    before: str
    after: str
    function: str | None = None

    def apply(self, original):
        source = original.decode("utf-8")
        start, end = 0, len(source)
        if self.function:
            anchor = f"fn {self.function}("
            if source.count(anchor) != 1:
                raise RuntimeError(f"{self.path}: function target is not unique")
            start = source.index(anchor)
            ends = [position for marker in ("\nfn ", "\npub(super) fn ")
                    if (position := source.find(marker, start + len(anchor))) >= 0]
            end = min(ends, default=len(source))
        region = source[start:end]
        if region.count(self.before) != 1:
            raise RuntimeError(f"{self.path}: mutation target must occur exactly once")
        return (source[:start] + region.replace(self.before, self.after, 1) + source[end:]).encode("utf-8")


def edit(name, before, after, function=None):
    return Edit(CLI + name + ".rs", before, after, function)


# Redundant integrity checks are bypassed together as one named mechanism.
# Each replacement still requires exactly one source target.
CASES = {
    "C10-SYSTEM-SHADOW": ("grapt-cli", "native_machine_shadow_refuses_a_first_user_entry", "a first USER entry was accepted behind a shadowing System command", [
        edit("user_path_resolution", 'Ok(_) => return Err(format!("System Path shadows {name} before this installation\'s USER Path entry; remove or reorder that conflicting System Path command before retrying. System Path was not changed")),', 'Ok(_) => {},')]),
    "C10-PATH-PERCENT": ("grapt-cli", "percent_directories_cannot_change_meaning", "expandable Path accepted a literal percent directory", [
        edit("user_path", "if current.kind == Some(true) && entry.contains('%') {", "if false && current.kind == Some(true) && entry.contains('%') {")]),
    "C10-PATH-PRECEDENCE": ("grapt-cli", "a_preexisting_command_directory_must_have", "shadowed command directory was accepted", [
        edit("user_path", ".is_none_or(|first| normalize(first) != normalize(entry))", ".is_none_or(|first| false && normalize(first) != normalize(entry))")]),
    "C10-RESOURCE-INVENTORY": ("grapt-cli", "resource_corruption_missing_extra_link", "extra must refuse", [
        edit("stateengine_package", "if inventory(root)? != expected {", "if false && inventory(root)? != expected {")]),
    "C10-LAUNCHER-OWNERSHIP": ("grapt-cli", "fixed_se_launcher_resolves_current", "unknown fixed launcher replacement was accepted", [
        edit("fixed_launcher", "        || receipt.sha256 != sha", "        || false && receipt.sha256 != sha")]),
    "C10-PATH-TYPE": ("grapt-cli", "expand_strings_and_empty_segments_round_trip", "assertion `left == right` failed", [
        edit("user_path", "kind: Some(self.kind.unwrap_or(false)),", "kind: Some(false),")]),
    "C10-PATH-OWNERSHIP": ("grapt-cli", "committed_removal_preserves_foreign_edits", "assertion `left == right` failed", [
        edit("user_path", ".filter(|part| *part != record.entry)", ".filter(|part| !part.is_empty() && *part != record.entry)")]),
    "C10-ALIAS-OWNERSHIP": ("grapt-cli", "reinstall_is_idempotent_and_uninstall_retains", "user replacement was removed", [
        edit("launchers", "if link_matches(&path, &target(prefix, name))? {", "if std::fs::symlink_metadata(&path).is_ok() {")]),
    "UPR-DIRECTORY": ("grapt-cli", "linked_existing_version_and_bin", "linked version was accepted", [
        edit("io", "std::fs::symlink_metadata(&current)", "std::fs::metadata(&current)", "directory")]),
    "UPR-ACTIVATION-DIGEST": ("grapt-cli", "changed_installed_bytes_refuse", "corrupt installed bytes were accepted", [
        edit("transition", "    verified.recheck(&installed)?;", "    io::directory(&installed)?;", "activate_verified_with")]),
    "UPR-SNAPSHOT-METADATA": ("grapt-cli", "metadata_changes_refuse", "changed unnoticed", [
        edit("snapshot", "            || (!expected.is_dir()\n", "            || (false && !expected.is_dir()\n", "recheck")]),
    "UPR-SNAPSHOT-IDENTITY": ("grapt-cli", "replacing_an_input_with_identical_bytes", "assertion failed", [
        edit("snapshot", "if expected.dev() != named.dev()", "if false && expected.dev() != named.dev()", "recheck"),
        edit("snapshot", "|| expected.ino() != named.ino()", "|| false && expected.ino() != named.ino()", "recheck"),
        edit("snapshot", "|| expected.ctime() != named.ctime()", "|| false && expected.ctime() != named.ctime()", "recheck"),
        edit("snapshot", "|| expected.ctime_nsec() != named.ctime_nsec()", "|| false && expected.ctime_nsec() != named.ctime_nsec()", "recheck")]),
    "UPR-PREFIX-DIRECTORY": ("grapt-service", "linked_prefix_parent_refuses", "PrefixGuard::shared", [
        Edit(GUARD, "std::fs::symlink_metadata(&current)", "std::fs::metadata(&current)", "directory")]),
    "UPR-IDENTITY": ("grapt-cli", "digest_identity_signature_and_unsafe_path", "identity was accepted", [
        edit("package", "                || entry.source_commit != manifest.engine_version\n", "")]),
    "UPR-PROFILE": ("grapt-cli", "digest_identity_signature_and_unsafe_path", "profile was accepted", [
        edit("package", "                || entry.profile != manifest.profile\n", "")]),
    "UPR-DIGEST": ("grapt-cli", "digest_identity_signature_and_unsafe_path", "digest was accepted", [
        edit("package", "!sha.eq_ignore_ascii_case(&entry.sha256) || length", "false || length"),
        edit("package", "!= Some(binary.sha256.as_str())", "!= Some(binary.sha256.as_str()) && false")]),
    "UPR-SIGNATURE": ("grapt-cli", "digest_identity_signature_and_unsafe_path", "signature was accepted", [
        edit("package", 'signing.get("signed").and_then(Value::as_bool) != Some(false)',
             'false && signing.get("signed").and_then(Value::as_bool) != Some(false)')]),
    "UPR-RUNTIME": ("grapt-cli", "digest_identity_signature_and_unsafe_path", "runtime-incomplete was accepted", [
        edit("package", "if !assets.contains_key(required) {", "if false && !assets.contains_key(required) {")]),
    "UPR-RECEIPT": ("grapt-cli", "a_rehashed_receipt_still", "inconsistent receipt profile was accepted", [
        edit("package", '                || receipt.get("profile").and_then(Value::as_str) != Some(manifest.profile.as_str())\n', "")]),
    "UPR-PREDECESSOR": ("grapt-cli", "each_crash_boundary_recovers", "assertion `left == right` failed", [
        edit("transition", "previous: intent.from.clone(),", "previous: None,", "finish")]),
    "UPR-CORRUPTION": ("grapt-cli", "corrupted_intent_cannot_replace", "corrupted predecessor was accepted", [
        edit("transition", "        verify_predecessor(prefix, &intent)?;\n", "", "recover_verified")]),
    "UPR-HISTORY": ("grapt-cli", "a_hard_linked_history", "aliased history was accepted", [
        edit("transition", "                && metadata.nlink() == 1", "", "history"),
        edit("transition", "        || opened.nlink() != 1\n", "", "history"),
        edit("transition", "                && metadata.nlink() == 1\n", "", "activate_verified_with")]),
    "UPR-COPY": ("grapt-cli", "changed_input_leaves", "changed input was published", [
        edit("stage", "    package.recheck(&package.root)?;\n", "", "install"),
        edit("stage", 'if length != input.bytes || format!("{:x}", hasher.finalize()) != input.sha256 {',
             'if false && (length != input.bytes || format!("{:x}", hasher.finalize()) != input.sha256) {'),
        edit("package", 'if !sha.eq_ignore_ascii_case(&entry.sha256) || length != entry.bytes || !executable {',
             'if false && (!sha.eq_ignore_ascii_case(&entry.sha256) || length != entry.bytes || !executable) {')]),
    "UPR-LINK": ("grapt-cli", "package_symlink_and_fifo", "nonregular package was accepted", [
        edit("io", "if !before.is_file() || before.file_type().is_symlink() {",
             "if false && (!before.is_file() || before.file_type().is_symlink()) {", "regular"),
        edit("io", ".custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)",
             ".custom_flags(libc::O_NONBLOCK)", "regular"),
        edit("io", "if !opened.is_file() || before.dev() != opened.dev() || before.ino() != opened.ino() {",
             "if false && (!opened.is_file() || before.dev() != opened.dev() || before.ino() != opened.ino()) {", "regular")]),
    "UPR-CHECKER": ("grapt-cli", "an_unavailable_older_checker", "unavailable older checker was accepted", [
        edit("commands", "        compatible(&prefix, &target.commit, &profile).await?;\n", "")]),
    "UPR-BUSY": ("grapt-service", "all_shared_holders_must_close", "PrefixGuard::exclusive", [
        Edit(GUARD, "Err(std::fs::TryLockError::WouldBlock) => Err(PrefixError::Busy),",
             "Err(std::fs::TryLockError::WouldBlock) => Ok(Self { _file: file }),")]),
    "UPR-PREFIX-WRITE": ("grapt-service", "group_or_other_write_access", "PrefixGuard::shared", [
        Edit(GUARD, "            return Err(PrefixError::Writable);", "            // Mutation: accept an externally writable prefix.")]),
}


def run(crate, test, log, timeout):
    command = ["cargo", "test", "--locked", "-p", crate, "--lib", test,
               "--no-fail-fast", "--", "--nocapture"]
    with log.open("x") as stream:
        try:
            execute(command, stream, time.monotonic() + timeout, cwd=ROOT)
            return 0
        except subprocess.CalledProcessError as error:
            return error.returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--case", action="append", choices=CASES)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    if not 1 <= args.timeout <= 600:
        parser.error("per-command timeout must be between 1 and 600 seconds")
    if subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, timeout=30).strip():
        parser.error("mutation runner requires a clean committed checkout")
    args.output.mkdir(mode=0o700)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, timeout=30).decode().strip()
    rows = []
    def interrupted(_signal, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    try:
        for name in args.case or CASES:
            crate, test, diagnostic, edits = CASES[name]
            originals = {item.path: (ROOT / item.path).read_bytes() for item in edits}
            row = {"case": name, "test": test, "edits": len(edits), "restored": False}
            rows.append(row)
            try:
                modified = dict(originals)
                for item in edits:
                    modified[item.path] = item.apply(modified[item.path])
                for path, source in modified.items():
                    (ROOT / path).write_bytes(source)
                red = args.output / f"{name}-red.log"
                row["red_exit"] = run(crate, test, red, args.timeout)
                if row["red_exit"] != 101 or diagnostic not in red.read_text():
                    raise RuntimeError(f"{name}: expected named counterexample, inspect {red}")
            finally:
                for path, source in originals.items():
                    (ROOT / path).write_bytes(source)
                row["restored"] = all((ROOT / path).read_bytes() == source for path, source in originals.items())
            green = args.output / f"{name}-green.log"
            row["green_exit"] = run(crate, test, green, args.timeout)
            if not row["restored"] or row["green_exit"] != 0 or "1 passed; 0 failed" not in green.read_text():
                raise RuntimeError(f"{name}: restored witness did not pass")
            print(f"{name}: named red, restored green", flush=True)
    finally:
        (args.output / "receipt.json").write_text(json.dumps({"source_commit": commit,
            "class": "synthetic unit fixtures; no native package credit", "cases": rows}, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        print(f"UPR failed: {error}", file=sys.stderr)
        sys.exit(1)
