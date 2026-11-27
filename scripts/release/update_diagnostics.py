#!/usr/bin/env python3
"""Prepare a separate, explicitly diagnostic checkout for native UPR barriers.

No build or product execution occurs here. The shipping checkout is untouched.
"""

import argparse
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[2]
INSTALLATION = Path("crates/grapt-cli/src/installation")
DIAGNOSTIC = r'''//! UPR DIAGNOSTIC ONLY. Absent from the shipping candidate.
//!
//! The native fixture waits for this marker, then kills the exact updater or
//! replaces its input. Every activation and I/O owner is otherwise unchanged.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::Duration;

pub(super) fn pause(phase: &str) -> Result<(), String> {
    if std::env::var("GRAPT_UPR_DIAGNOSTIC_PHASE").ok().as_deref() != Some(phase) {
        return Ok(());
    }
    let root = PathBuf::from(std::env::var_os("GRAPT_UPR_DIAGNOSTIC_ROOT")
        .ok_or("UPR diagnostic root is missing")?);
    let metadata = std::fs::symlink_metadata(&root)
        .map_err(|error| format!("UPR diagnostic root: {error}"))?;
    if !root.is_absolute() || !metadata.is_dir() || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0 {
        return Err("UPR diagnostic root must be a private real directory".into());
    }
    let marker = root.join("phase");
    let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(marker) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(format!("UPR diagnostic marker: {error}")),
    };
    file.write_all(phase.as_bytes()).and_then(|()| file.sync_all())
        .map_err(|error| format!("UPR diagnostic marker sync: {error}"))?;
    super::io::sync_directory(&root)?;
    for _ in 0..600 {
        match std::fs::symlink_metadata(root.join("release")) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink()
                && metadata.len() == 0 => return Ok(()),
            Ok(_) => return Err("UPR diagnostic release must be an empty regular file".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(format!("UPR diagnostic release: {error}")),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("UPR diagnostic barrier exceeded 30 seconds".into())
}
'''


def replace(tree, name, before, after):
    path = tree / INSTALLATION / name
    source = path.read_text()
    if source.count(before) != 1:
        raise RuntimeError(f"{name}: diagnostic target must occur exactly once")
    path.write_text(source.replace(before, after, 1))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tree", required=True, type=Path, help="fresh diagnostic worktree path")
    parser.add_argument("--base", default="HEAD", help="exact committed shipping source to diagnose")
    parser.add_argument("--fifo-mutant", action="store_true",
                        help="also remove the regular-input nonblocking-open guard")
    parser.add_argument("--product-lifetime-mutant", action="store_true",
                        help="release the product prefix guard before its command runs")
    parser.add_argument("--hash-under-prefix-mutant", action="store_true",
                        help="restore expensive activation hashing inside prefix exclusion")
    args = parser.parse_args()
    if sum([args.fifo_mutant, args.product_lifetime_mutant, args.hash_under_prefix_mutant]) > 1:
        parser.error("choose one diagnostic mutation")
    if subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, timeout=30).strip():
        parser.error("source checkout must be clean and committed")
    base = subprocess.check_output(["git", "rev-parse", "--verify", args.base + "^{commit}"],
                                   cwd=ROOT, timeout=30).decode().strip()
    tree = args.tree.resolve()
    if tree.exists():
        parser.error("diagnostic tree must not already exist")
    subprocess.run(["git", "worktree", "add", "--detach", str(tree), base], cwd=ROOT, timeout=120, check=True)
    replace(tree, "mod.rs", '#[cfg(target_os = "linux")]\nmod io;',
            '#[cfg(target_os = "linux")]\nmod diagnostic;\n#[cfg(target_os = "linux")]\nmod io;')
    (tree / INSTALLATION / "diagnostic.rs").write_text(DIAGNOSTIC)
    replace(tree, "transition.rs", '    activate_verified_with(prefix, verified, |_| Ok(()))', '''    activate_verified_with(prefix, verified, |boundary| {
        super::diagnostic::pause(match boundary {
            Boundary::Intent => "intent",
            Boundary::Pointer => "pointer",
            Boundary::State => "state",
        })
    })''')
    replace(tree, "io.rs", '    let file = OpenOptions::new()\n        .read(true)\n        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)', '''    if std::env::var_os("GRAPT_UPR_DIAGNOSTIC_INPUT")
        .is_some_and(|input| Path::new(&input) == path) {
        super::diagnostic::pause("open")?;
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)''')
    replace(tree, "stage.rs", '            .map_err(|error| format!("staging write failed: {:?}", error.kind()))?;', '''            .map_err(|error| format!("staging write failed: {:?}", error.kind()))?;
        if input.relative == Path::new("grapt-server") && length == count as u64 {
            super::diagnostic::pause("copy")?;
        }''')
    replace(tree, "commands.rs", '    let verified_current = transition::verified_current(&prefix)?;', '''    super::diagnostic::pause("staging")?;
    let verified_current = transition::verified_current(&prefix)?;''')
    if args.fifo_mutant:
        replace(tree, "io.rs", ".custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)",
                ".custom_flags(libc::O_NOFOLLOW)")
    if args.hash_under_prefix_mutant:
        # Move the real source/active/staged verification back under prefix EX.
        # The separate preparation witness proves that starts now wait there.
        guard = '    let _guard = PrefixGuard::exclusive(&prefix).map_err(|error| error.to_string())?;'
        replace(tree, "commands.rs", guard + '\n', '')
        replace(tree, "commands.rs", '    super::diagnostic::pause("staging")?;',
                guard + '\n    super::diagnostic::pause("staging")?;')
    paths = [str(INSTALLATION / name) for name in ["mod.rs", "diagnostic.rs", "transition.rs", "io.rs", "stage.rs", "commands.rs"]]
    if args.product_lifetime_mutant:
        relative = "crates/grapt-product/src/installation.rs"
        path = tree / relative
        source = path.read_text()
        before = "    Ok(Some(guard))"
        if source.count(before) != 1:
            raise RuntimeError("product lifetime guard target must occur exactly once")
        path.write_text(source.replace(before, "    drop(guard);\n    Ok(None)", 1))
        paths.append(relative)
    subprocess.run(["rustfmt", "--edition", "2021", *(str(tree / path) for path in paths)],
                   cwd=tree, timeout=30, check=True)
    subprocess.run(["git", "add", "--", *paths], cwd=tree, timeout=30, check=True)
    body = ("test(update): prepare diagnostic phase barriers\n\n"
            "DIAGNOSTIC ONLY — NEVER THE SHIPPING CANDIDATE.\n"
            f"Base {base}. Six unique-target barriers preserve the existing owners,\n"
            "and wait only in this separate native UPR diagnostic checkout.\n"
            f"FIFO nonblocking guard removed: {args.fifo_mutant}.\n"
            f"Product lifetime guard released early: {args.product_lifetime_mutant}.\n"
            f"Preparation hashing under prefix exclusion: {args.hash_under_prefix_mutant}.\n"
            "Gates: formatted; unbuilt, no witness credit.\n")
    subprocess.run(["git", "commit", "-F", "-", "--", *paths], cwd=tree,
                   input=body, text=True, timeout=30, check=True)
    print(f"Diagnostic checkout prepared at {tree}; build and protect its CLI before using it.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
