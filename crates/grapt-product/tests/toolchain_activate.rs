//! The product seam for `toolchain activate`: the host's own triple, the
//! committed pin, and the boundary's receipts on stdout and stderr.
//!
//! Every test runs the real binary against a fresh scene on this host: the
//! native triple activates through the boundary, a foreign triple refuses at
//! the seam, a malformed line refuses with usage, and a missing pin refuses
//! naming the fact. The archive bytes are written by this test's own minimal
//! ustar writer, so nothing is owed beyond the committed pin.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const BIN: &str = env!("CARGO_BIN_EXE_grapt-server");

fn native_triple() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    format!("{}-pc-{os}", std::env::consts::ARCH)
}

/// One minimal ustar stream: a single file entry plus the two-block
/// terminator. The header checksum is computed the way the decoder verifies
/// it: the field read as spaces.
fn ustar(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut header = [0u8; 512];
    header[..name.len()].copy_from_slice(name.as_bytes());
    let size = format!("{:07o}", bytes.len());
    header[124..124 + size.len()].copy_from_slice(size.as_bytes());
    header[124 + size.len()] = 0;
    header[148] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let mut sum = 0u64;
    for (index, byte) in header.iter().enumerate() {
        sum += if (140..148).contains(&index) {
            0x20
        } else {
            u64::from(*byte)
        };
    }
    let checksum = format!("{:07o}", sum);
    header[140..140 + checksum.len()].copy_from_slice(checksum.as_bytes());
    header[140 + checksum.len()] = 0;
    let padded = bytes.len().div_ceil(512) * 512;
    let mut data = bytes.to_vec();
    data.resize(padded, 0);
    let mut out = header.to_vec();
    out.extend_from_slice(&data);
    out.extend_from_slice(&[0u8; 1024]);
    out
}

fn pin_text(version: &str, target: &str, digest: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schema": "secode.toolchain.pins.v1",
        "tool": "ripgrep",
        "version": version,
        "pins": {
            target: {
                "archive_sha256": digest,
                "entry": "rg.exe",
                "format": "tar",
                "source": "archives/rg.tar",
            }
        },
    }))
    .unwrap_or_else(|error| panic!("pin text: {error}"))
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "toolchain-test")
        .env("GIT_AUTHOR_EMAIL", "toolchain-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "toolchain-test")
        .env("GIT_COMMITTER_EMAIL", "toolchain-test@example.invalid")
        .output()
        .unwrap_or_else(|error| panic!("git is unavailable: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fresh_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("grapt-tc-act-{label}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap_or_else(|error| panic!("create root: {error}"));
    root
}

/// A committed scene: the pin and the archive on disk and in git, so the
/// pin attributes to a real commit of a real worktree.
fn activation_scene(label: &str, version: &str, triple: &str, entry_bytes: &[u8]) -> PathBuf {
    let root = fresh_root(label);
    let archive = ustar("rg.exe", entry_bytes);
    let digest = grapt_toolchain::Sha256::of(&archive).hex();
    fs::create_dir_all(root.join("archives")).unwrap_or_else(|error| panic!("archives: {error}"));
    fs::write(root.join("archives/rg.tar"), archive)
        .unwrap_or_else(|error| panic!("archive: {error}"));
    fs::write(root.join("pins.json"), pin_text(version, triple, &digest))
        .unwrap_or_else(|error| panic!("pin: {error}"));
    git(&root, &["init", "-q"]);
    git(&root, &["add", "--", "pins.json", "archives/rg.tar"]);
    git(
        &root,
        &["commit", "-q", "-m", "seam test: pin and release input"],
    );
    root
}

fn run_cli(root: &Path, triple: &str) -> Output {
    Command::new(BIN)
        .args(["toolchain", "activate", "--root"])
        .arg(root)
        .args(["--target", triple, "--pin", "pins.json"])
        .output()
        .unwrap_or_else(|error| panic!("the cli failed to launch: {error}"))
}

#[test]
fn the_seam_activates_the_pinned_tool_for_this_host() {
    let entry = b"fake-ripgrep-binary-bytes";
    let root = activation_scene("happy", "15.1.0", &native_triple(), entry);
    let out = run_cli(&root, &native_triple());
    assert!(
        out.status.success(),
        "the seam must activate this host: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let receipt: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|error| panic!("receipt: {error}"));
    assert_eq!(receipt["schema"], "grapt.toolchain.receipt.v1");
    assert_eq!(receipt["outcome"], "activated");
    assert_eq!(receipt["tool"], "ripgrep");
    assert_eq!(
        receipt["binary_sha256"],
        grapt_toolchain::Sha256::of(entry).hex()
    );
    let commit = receipt["commit"]
        .as_str()
        .unwrap_or_else(|| panic!("the receipt must carry the pin commit"));
    assert_eq!(commit.len(), 40, "the commit is a full sha: {commit}");
    let canonical = root
        .join("tools")
        .join("ripgrep")
        .join("bin")
        .join("rg.exe");
    let bytes = fs::read(&canonical).unwrap_or_else(|error| panic!("canonical: {error}"));
    assert_eq!(bytes, &entry[..], "the canonical bytes are the entry bytes");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_seam_refuses_a_target_that_is_not_this_host() {
    let root = activation_scene(
        "foreign",
        "15.1.0",
        &native_triple(),
        b"fake-ripgrep-binary-bytes",
    );
    let foreign = format!("{}-pc-foreignos", std::env::consts::ARCH);
    let out = run_cli(&root, &foreign);
    assert_eq!(out.status.code(), Some(2), "a foreign triple must refuse");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--target must describe this host"),
        "the seam must name the host boundary: {stderr}"
    );
    assert!(
        !root.join("tools").exists(),
        "the refusal is at the seam: the boundary is never reached"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_malformed_seam_line_refuses_with_usage() {
    let root = fresh_root("usage");
    let out = Command::new(BIN)
        .args(["toolchain", "activate", "--root"])
        .arg(&root)
        .output()
        .unwrap_or_else(|error| panic!("the cli failed to launch: {error}"));
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("product launch refused"),
        "the refusal is named: {stderr}"
    );
    assert!(
        stderr.contains("usage:"),
        "the usage names the admitted verbs: {stderr}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_seam_refuses_a_missing_pin_naming_the_fact() {
    let root = fresh_root("missing-pin");
    let out = run_cli(&root, &native_triple());
    assert_eq!(out.status.code(), Some(2));
    let receipt: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|error| panic!("receipt: {error}"));
    assert_eq!(receipt["schema"], "grapt.toolchain.receipt.v1");
    assert_eq!(receipt["outcome"], "refused");
    assert_eq!(receipt["reason"], "pin-missing");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("the pin file does not exist"),
        "the refusal names the fact: {stderr}"
    );
    let _ = fs::remove_dir_all(&root);
}
