//! Synthetic packages exercise refusal and crash boundaries, never native-build credit.

#[path = "rename_tests.rs"]
mod rename_tests;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{package::Package, stage, transition};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);
const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";
const C: &str = "3333333333333333333333333333333333333333";
const NAMES: [&str; 5] = [
    "grapt-server",
    "grapt",
    "grapt-engined",
    "grapt-migrate",
    "grapt-tui",
];

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let root = std::env::temp_dir().join(format!(
            "grapt-install-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        Ok(Self(root))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn fixture(root: &Path, identity: &str) -> Result<PathBuf> {
    let package = root.join(identity);
    std::fs::create_dir(&package)?;
    let binary = b"synthetic fixture; never execute";
    let hash = digest(binary);
    let mut outputs = serde_json::Map::new();
    for name in NAMES {
        std::fs::write(package.join(name), binary)?;
        std::fs::set_permissions(package.join(name), std::fs::Permissions::from_mode(0o700))?;
        outputs.insert(name.into(), json!({"bytes":binary.len(),"sha256":hash}));
    }
    let receipt = serde_json::to_vec(&json!({
        "schema":"secode.release.build.v1", "source_commit":identity,
        "target":"x86_64-unknown-linux-gnu", "tree_dirty":false, "profile":"debug",
        "commands":[{"produces":NAMES}], "outputs":outputs,
    }))?;
    std::fs::write(package.join("build.json"), &receipt)?;
    let mut binaries = serde_json::Map::new();
    let mut signing = serde_json::Map::new();
    for name in NAMES {
        binaries.insert(
            name.into(),
            json!({"sha256":hash, "bytes":binary.len(), "source_commit":identity,
            "profile":"debug", "target":"x86_64-unknown-linux-gnu",
            "build_receipt":"build.json", "build_receipt_sha256":digest(&receipt)}),
        );
        signing.insert(
            name.into(),
            json!({"signed":false, "reason":"synthetic unsigned test"}),
        );
    }
    let runtime = package.join("plugin-runtime");
    std::fs::create_dir(&runtime)?;
    std::fs::write(runtime.join("node"), binary)?;
    std::fs::set_permissions(runtime.join("node"), std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(runtime.join("LICENSE"), b"synthetic license")?;
    std::fs::write(runtime.join("runtime.json"), b"{}")?;
    let bridge = package.join("plugin-bridge");
    std::fs::create_dir(&bridge)?;
    std::fs::write(bridge.join("index.mjs"), b"synthetic bridge")?;
    let manifest = json!({"schema":"secode.release.package.v1", "target":"x86_64-unknown-linux-gnu",
    "profile":"debug", "engine_version":identity, "binaries":binaries, "signing":signing, "publisher":null,
    "entry_points":NAMES, "plugin_runtime":{"status":"included", "assets":{
        "plugin-runtime/node":{"sha256":hash,"bytes":binary.len()},
        "plugin-runtime/LICENSE":{"sha256":digest(b"synthetic license"),"bytes":17},
        "plugin-runtime/runtime.json":{"sha256":digest(b"{}"),"bytes":2},
        "plugin-bridge/index.mjs":{"sha256":digest(b"synthetic bridge"),"bytes":16},
    }}});
    std::fs::write(
        package.join("release-manifest.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    std::fs::write(package.join("QUICKSTART.md"), b"synthetic guide")?;
    Ok(package)
}

fn change_manifest(root: &Path, change: impl FnOnce(&mut Value)) -> Result {
    let path = root.join("release-manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    change(&mut manifest);
    std::fs::write(path, serde_json::to_vec(&manifest)?)?;
    Ok(())
}

fn prefix(root: &Path) -> Result<PathBuf> {
    let prefix = root.join("installation");
    std::fs::create_dir(&prefix)?;
    std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700))?;
    Ok(prefix)
}

#[test]
fn package_and_staged_inputs_reverify_with_plugin_and_guide_paths() -> Result {
    let root = Scratch::new()?;
    let package = Package::verify(&fixture(&root.0, A)?)?;
    let prefix = prefix(&root.0)?;
    stage::install(&prefix, &package)?;
    let installed = prefix.join("versions").join(A).join("bin");
    assert_eq!(Package::verify(&installed)?.commit, A);
    for name in [
        "build.json",
        "plugin-runtime/node",
        "plugin-bridge/index.mjs",
        "QUICKSTART.md",
    ] {
        assert_eq!(
            std::fs::read(installed.join(name))?,
            std::fs::read(package.root.join(name))?
        );
    }
    stage::install(&prefix, &package)?;
    Ok(())
}

#[test]
fn digest_identity_signature_and_unsafe_path_refuse_before_activation() -> Result {
    for mutation in [
        "digest",
        "identity",
        "signature",
        "path",
        "receipt",
        "size",
        "profile",
        "target",
        "manifest-profile",
        "runtime-incomplete",
    ] {
        let root = Scratch::new()?;
        let package = fixture(&root.0, A)?;
        change_manifest(&package, |value| match mutation {
            "digest" => value["binaries"]["grapt"]["sha256"] = json!("0".repeat(64)),
            "identity" => value["binaries"]["grapt"]["source_commit"] = json!(B),
            "signature" => value["signing"]["grapt"]["signed"] = json!(true),
            "profile" => value["binaries"]["grapt"]["profile"] = json!("release"),
            "target" => value["binaries"]["grapt"]["target"] = json!("x86_64-pc-windows-msvc"),
            "manifest-profile" => value["profile"] = json!("release"),
            "runtime-incomplete" => {
                if let Some(assets) = value["plugin_runtime"]["assets"].as_object_mut() {
                    assets.remove("plugin-runtime/LICENSE");
                }
            }
            "path" => {
                value["plugin_runtime"]["assets"]["plugin-runtime/../../escape"] =
                    json!({"sha256":"0".repeat(64),"bytes":0})
            }
            "receipt" => value["binaries"]["grapt"]["build_receipt"] = json!("../receipt"),
            _ => value["binaries"]["grapt"]["bytes"] = json!(u64::MAX),
        })?;
        assert!(
            Package::verify(&package).is_err(),
            "{mutation} was accepted"
        );
        assert!(!root.0.join("installation/current").exists());
    }
    Ok(())
}

#[test]
fn a_rehashed_receipt_still_must_match_the_declared_profile() -> Result {
    let root = Scratch::new()?;
    let package = fixture(&root.0, A)?;
    let path = package.join("build.json");
    let mut receipt: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    receipt["profile"] = json!("release");
    let receipt = serde_json::to_vec(&receipt)?;
    std::fs::write(path, &receipt)?;
    change_manifest(&package, |manifest| {
        for name in NAMES {
            manifest["binaries"][name]["build_receipt_sha256"] = json!(digest(&receipt));
        }
    })?;
    let refusal = Package::verify(&package)
        .err()
        .ok_or("inconsistent receipt profile was accepted")?;
    assert!(
        refusal.contains("build receipt does not attest"),
        "{refusal}"
    );
    Ok(())
}

#[test]
fn package_symlink_and_fifo_are_refused_without_opening_them() -> Result {
    for fifo in [false, true] {
        let root = Scratch::new()?;
        let package = fixture(&root.0, A)?;
        std::fs::remove_file(package.join("grapt"))?;
        if fifo {
            let status = std::process::Command::new("mkfifo")
                .arg(package.join("grapt"))
                .status()?;
            assert!(status.success());
        } else {
            std::os::unix::fs::symlink(package.join("grapt-server"), package.join("grapt"))?;
        }
        let refused = Package::verify(&package)
            .err()
            .ok_or("nonregular package was accepted")?;
        assert!(refused.contains("regular file"), "{refused}");
    }
    Ok(())
}

#[test]
fn changed_input_leaves_current_unchanged_and_partial_can_be_retried() -> Result {
    let root = Scratch::new()?;
    let package = Package::verify(&fixture(&root.0, A)?)?;
    let next = Package::verify(&fixture(&root.0, B)?)?;
    let prefix = prefix(&root.0)?;
    stage::install(&prefix, &package)?;
    transition::activate(&prefix, A)?;
    std::fs::write(next.root.join("grapt"), b"changed after verification")?;
    assert!(
        stage::install(&prefix, &next).is_err(),
        "changed input was published"
    );
    assert_eq!(transition::pointer(&prefix)?.as_deref(), Some(A));
    assert!(!prefix.join("versions").join(B).exists());
    std::fs::write(next.root.join("grapt"), b"synthetic fixture; never execute")?;
    assert!(
        stage::install(&prefix, &next).is_err(),
        "a stale snapshot was reused"
    );
    let next = Package::verify(&next.root)?;
    stage::install(&prefix, &next)?;
    transition::activate(&prefix, B)?;
    assert_eq!(
        transition::state(&prefix)?
            .ok_or("missing state")?
            .previous
            .as_deref(),
        Some(A)
    );
    Ok(())
}

#[test]
fn each_crash_boundary_recovers_once_and_preserves_the_actual_predecessor() -> Result {
    for stop in [
        transition::Boundary::Intent,
        transition::Boundary::Pointer,
        transition::Boundary::State,
    ] {
        let root = Scratch::new()?;
        let prefix = prefix(&root.0)?;
        for identity in [A, B, C] {
            stage::install(&prefix, &Package::verify(&fixture(&root.0, identity)?)?)?;
        }
        transition::activate(&prefix, A)?;
        transition::activate(&prefix, B)?;
        let failure = transition::activate_with(&prefix, C, |boundary| {
            if std::mem::discriminant(&boundary) == std::mem::discriminant(&stop) {
                Err("injected crash".into())
            } else {
                Ok(())
            }
        });
        assert!(failure.is_err());
        transition::recover(&prefix)?;
        let recovered = transition::state(&prefix)?.ok_or("missing state")?;
        let (current, previous) = if matches!(stop, transition::Boundary::Intent) {
            (B, A)
        } else {
            (C, B)
        };
        assert_eq!(recovered.current, current);
        assert_eq!(recovered.previous.as_deref(), Some(previous));
        transition::recover(&prefix)?;
        assert!(!prefix.join("transition.json").exists());
        assert_eq!(
            transition::state(&prefix)?.ok_or("missing state")?.current,
            current
        );
    }
    Ok(())
}

#[test]
fn a_hard_linked_history_is_refused_before_switching() -> Result {
    let root = Scratch::new()?;
    let prefix = prefix(&root.0)?;
    for identity in [A, B] {
        stage::install(&prefix, &Package::verify(&fixture(&root.0, identity)?)?)?;
    }
    transition::activate(&prefix, A)?;
    let alias = root.0.join("history-alias");
    std::fs::hard_link(prefix.join("history.jsonl"), &alias)?;
    let before = std::fs::read(&alias)?;
    assert!(
        transition::activate(&prefix, B).is_err(),
        "aliased history was accepted"
    );
    assert_eq!(std::fs::read(alias)?, before);
    assert_eq!(transition::pointer(&prefix)?.as_deref(), Some(A));
    Ok(())
}

#[test]
fn corrupted_intent_cannot_replace_the_recorded_predecessor() -> Result {
    for stop in [transition::Boundary::Pointer, transition::Boundary::State] {
        let root = Scratch::new()?;
        let prefix = prefix(&root.0)?;
        for identity in [A, B, C] {
            stage::install(&prefix, &Package::verify(&fixture(&root.0, identity)?)?)?;
        }
        transition::activate(&prefix, A)?;
        transition::activate(&prefix, B)?;
        let interrupted = transition::activate_with(&prefix, C, |boundary| {
            if std::mem::discriminant(&boundary) == std::mem::discriminant(&stop) {
                Err("injected crash".into())
            } else {
                Ok(())
            }
        });
        assert!(interrupted.is_err());
        let intent = prefix.join("transition.json");
        let original = std::fs::read(&intent)?;
        let mut corrupt: Value = serde_json::from_slice(&original)?;
        corrupt["from"] = json!(A);
        let corrupt = serde_json::to_vec(&corrupt)?;
        std::fs::write(&intent, &corrupt)?;
        let state = std::fs::read(prefix.join("state.json"))?;
        let history = std::fs::read(prefix.join("history.jsonl"))?;
        let pointer = std::fs::read_link(prefix.join("current"))?;
        let error = transition::recover(&prefix)
            .err()
            .ok_or("corrupted predecessor was accepted")?;
        assert!(error.contains("predecessor"), "{error}");
        assert_eq!(std::fs::read(prefix.join("state.json"))?, state);
        assert_eq!(std::fs::read(prefix.join("history.jsonl"))?, history);
        assert_eq!(std::fs::read_link(prefix.join("current"))?, pointer);
        assert_eq!(std::fs::read(&intent)?, corrupt);
        std::fs::write(intent, original)?;
        transition::recover(&prefix)?;
        let restored = transition::state(&prefix)?.ok_or("missing recovered state")?;
        assert_eq!(restored.current, C);
        assert_eq!(restored.previous.as_deref(), Some(B));
    }
    Ok(())
}

#[test]
fn first_install_interruption_has_no_invented_predecessor() -> Result {
    for stop in [
        transition::Boundary::Intent,
        transition::Boundary::Pointer,
        transition::Boundary::State,
    ] {
        let root = Scratch::new()?;
        let prefix = prefix(&root.0)?;
        stage::install(&prefix, &Package::verify(&fixture(&root.0, A)?)?)?;
        assert!(transition::activate_with(&prefix, A, |boundary| {
            if std::mem::discriminant(&boundary) == std::mem::discriminant(&stop) {
                Err("injected first-install crash".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        transition::recover(&prefix)?;
        let state = transition::state(&prefix)?;
        if matches!(stop, transition::Boundary::Intent) {
            assert!(state.is_none(), "abandoned first install fabricated state");
            assert!(transition::pointer(&prefix)?.is_none());
        } else {
            let state = state.ok_or("first install lost its activated state")?;
            assert_eq!(state.current, A);
            assert!(
                state.previous.is_none(),
                "first install fabricated a predecessor"
            );
        }
        transition::recover(&prefix)?;
        assert!(!prefix.join("transition.json").exists());
    }
    Ok(())
}

#[test]
fn a_lost_state_is_not_reconstructed_from_the_intent() -> Result {
    let root = Scratch::new()?;
    let prefix = prefix(&root.0)?;
    for identity in [A, B] {
        stage::install(&prefix, &Package::verify(&fixture(&root.0, identity)?)?)?;
    }
    transition::activate(&prefix, A)?;
    assert!(transition::activate_with(&prefix, B, |boundary| {
        if matches!(boundary, transition::Boundary::Pointer) {
            Err("injected crash".into())
        } else {
            Ok(())
        }
    })
    .is_err());
    let path = prefix.join("state.json");
    let state = std::fs::read(&path)?;
    let intent = std::fs::read(prefix.join("transition.json"))?;
    let history = std::fs::read(prefix.join("history.jsonl"))?;
    std::fs::remove_file(&path)?;
    let refusal = transition::recover(&prefix)
        .err()
        .ok_or("missing predecessor evidence was accepted")?;
    assert!(refusal.contains("predecessor"), "{refusal}");
    assert!(!path.exists(), "recovery fabricated missing state");
    assert_eq!(std::fs::read(prefix.join("transition.json"))?, intent);
    assert_eq!(std::fs::read(prefix.join("history.jsonl"))?, history);
    assert_eq!(transition::pointer(&prefix)?.as_deref(), Some(B));
    std::fs::write(path, state)?;
    transition::recover(&prefix)?;
    let recovered = transition::state(&prefix)?.ok_or("missing repaired state")?;
    assert_eq!(recovered.current, B);
    assert_eq!(recovered.previous.as_deref(), Some(A));
    Ok(())
}

#[tokio::test]
async fn an_unavailable_older_checker_refuses_without_switching() -> Result {
    let root = Scratch::new()?;
    let prefix = prefix(&root.0)?;
    for identity in [A, B] {
        stage::install(&prefix, &Package::verify(&fixture(&root.0, identity)?)?)?;
        transition::activate(&prefix, identity)?;
    }
    let before = std::fs::read(prefix.join("state.json"))?;
    let history = std::fs::read(prefix.join("history.jsonl"))?;
    // These labelled synthetic files cannot execute as a product checker.
    let result = super::commands::run(&[
        "revert".into(),
        "--prefix".into(),
        prefix.to_string_lossy().into_owned(),
        "--profile".into(),
        root.0.join("absent-profile").to_string_lossy().into_owned(),
    ])
    .await;
    let refusal = result
        .err()
        .ok_or("unavailable older checker was accepted")?;
    assert!(
        refusal.contains("read-only profile check cannot start"),
        "{refusal}"
    );
    assert_eq!(transition::pointer(&prefix)?.as_deref(), Some(B));
    assert_eq!(std::fs::read(prefix.join("state.json"))?, before);
    assert_eq!(std::fs::read(prefix.join("history.jsonl"))?, history);
    assert!(!prefix.join("transition.json").exists());
    assert!(!root.0.join("absent-profile").exists());
    Ok(())
}

#[path = "directory_tests.rs"]
mod directory_tests;

#[path = "snapshot_tests.rs"]
mod snapshot_tests;
#[test]
fn windows_pointer_adapter_distinguishes_absence_and_corruption() -> Result {
    let root = Scratch::new()?;
    assert_eq!(super::windows_pointer::read(&root.0)?, None);
    std::fs::write(root.0.join("current"), b"partial\n")?;
    assert!(super::windows_pointer::read(&root.0).is_err());
    std::fs::remove_file(root.0.join("current"))?;
    std::fs::create_dir(root.0.join("current"))?;
    assert!(super::windows_pointer::read(&root.0).is_err());
    Ok(())
}

#[test]
fn windows_pointer_adapter_replaces_the_name_without_writing_an_alias() -> Result {
    let root = Scratch::new()?;
    super::windows_pointer::publish(&root.0, A)?;
    let outside = root.0.join("outside-alias");
    std::fs::hard_link(root.0.join("current"), &outside)?;
    let original = std::fs::read(&outside)?;
    super::windows_pointer::publish(&root.0, B)?;
    assert_eq!(super::windows_pointer::read(&root.0)?.as_deref(), Some(B));
    assert_eq!(std::fs::read(&outside)?, original);
    assert!(!root.0.join("current.new").exists());
    Ok(())
}

#[test]
fn windows_pointer_adapter_refuses_a_link_and_an_invalid_publication() -> Result {
    let root = Scratch::new()?;
    let outside = root.0.join("outside");
    std::fs::write(&outside, format!("{A}\n"))?;
    std::os::unix::fs::symlink(&outside, root.0.join("current"))?;
    assert!(super::windows_pointer::read(&root.0).is_err());
    assert!(super::windows_pointer::publish(&root.0, "../outside").is_err());
    assert_eq!(std::fs::read(&outside)?, format!("{A}\n").as_bytes());
    assert!(std::fs::symlink_metadata(root.0.join("current"))?
        .file_type()
        .is_symlink());
    Ok(())
}

#[path = "prune_tree_tests.rs"]
mod prune_tree_tests;

#[test]
fn prune_history_preserves_activation_order_including_reverts() -> Result {
    let root = Scratch::new()?;
    let row = |from: Option<&str>, to: &str| {
        serde_json::to_string(&json!({
            "schema":"secode.install.transition.v1", "from":from, "to":to
        }))
    };
    std::fs::write(
        root.0.join("history.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            row(None, A)?,
            row(Some(A), B)?,
            row(Some(B), A)?
        ),
    )?;
    assert_eq!(transition::activations(&root.0)?, [A, B, A]);
    Ok(())
}

#[test]
fn prune_history_distinguishes_missing_from_invalid_or_unreadable() -> Result {
    let root = Scratch::new()?;
    assert!(transition::activations(&root.0)?.is_empty());
    let path = root.0.join("history.jsonl");
    std::fs::write(&path, b"partial activation record")?;
    assert!(transition::activations(&root.0).is_err());
    std::fs::remove_file(&path)?;
    std::fs::create_dir(&path)?;
    assert!(transition::activations(&root.0).is_err());
    Ok(())
}

#[path = "prune_tests.rs"]
mod prune_tests;

#[path = "stateengine_tests.rs"]
mod stateengine_tests;

#[path = "frontend_tests.rs"]
mod frontend_tests;
