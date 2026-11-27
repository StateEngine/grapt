//! Native Windows staging uses the shared verifier at the final inactive name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use sha2::{Digest, Sha256};

use super::{io, package::Package, stage, transition, windows_pointer};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";
const NAMES: [&str; 5] = [
    "grapt-server.exe",
    "grapt.exe",
    "grapt-engined.exe",
    "grapt-migrate.exe",
    "grapt-tui.exe",
];
static NEXT: AtomicU64 = AtomicU64::new(0);

#[path = "frontend_tests.rs"]
mod frontend_tests;
#[path = "portable_tests.rs"]
mod portable_tests;
#[path = "windows_signing_tests.rs"]
mod signing_tests;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "grapt-windows-stage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

/// The install-permission reader runs System32 programs from `%SystemRoot%`;
/// a witness that re-runs itself under a cleared environment passes it on.
fn system_root() -> std::ffi::OsString {
    std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into())
}

fn fixture(root: &Path, identity: &str) -> Result<PathBuf> {
    let bin = root.join(identity);
    std::fs::create_dir(&bin)?;
    let mut outputs = BTreeMap::new();
    for name in NAMES {
        let bytes = format!("fixture-{identity}-{name}");
        io::write_new(&bin.join(name), bytes.as_bytes())?;
        outputs.insert(name, json!({"sha256":format!("{:x}", Sha256::digest(bytes.as_bytes())), "bytes":bytes.len()}));
    }
    let receipt = serde_json::to_vec(&json!({
        "schema":"secode.release.build.v1", "source_commit":identity,
        "target":"x86_64-pc-windows-msvc", "profile":"debug", "tree_dirty":false,
        "commands":[{"produces":NAMES}], "outputs":outputs,
    }))?;
    io::write_new(&bin.join("build-receipt.json"), &receipt)?;
    let receipt_sha = format!("{:x}", Sha256::digest(&receipt));
    let mut binaries = BTreeMap::new();
    let mut signing = BTreeMap::new();
    for name in NAMES {
        binaries.insert(
            name,
            json!({
                "target":"x86_64-pc-windows-msvc", "profile":"debug",
                "sha256":outputs[name]["sha256"], "bytes":outputs[name]["bytes"],
                "source_commit":identity, "build_receipt":"build-receipt.json",
                "build_receipt_sha256":receipt_sha,
            }),
        );
        signing.insert(
            name,
            json!({"signed":false,"reason":"disposable local test fixture"}),
        );
    }
    io::write_new(
        &bin.join("release-manifest.json"),
        &serde_json::to_vec(&json!({
            "schema":"secode.release.package.v1", "target":"x86_64-pc-windows-msvc",
            "profile":"debug", "engine_version":identity, "binaries":binaries,
            "entry_points":NAMES,"signing":signing,"publisher":null,
            "plugin_runtime":{"status":"not-included"},
        }))?,
    )?;
    Ok(bin)
}

#[test]
fn staging_verifies_final_name_before_any_activation() -> Result {
    let root = Scratch::new()?;
    let prefix = root.0.join("installation");
    std::fs::create_dir(&prefix)?;
    let source = Package::verify(&fixture(&root.0, A)?)?;
    let _writer = stage::writer(&prefix)?;
    let installed = stage::install(&prefix, &source)?;
    assert_eq!(installed.root, prefix.join("versions").join(A).join("bin"));
    installed.recheck(&installed.root)?;
    assert_eq!(windows_pointer::read(&prefix)?, None);
    assert!(!prefix.join("versions/.partial").exists());
    assert!(std::fs::rename(
        installed.root.parent().ok_or("no version parent")?,
        prefix.join("moved-version")
    )
    .is_err());
    Ok(())
}

#[test]
fn invalid_final_name_bytes_are_never_an_activation_target() -> Result {
    let root = Scratch::new()?;
    let prefix = root.0.join("installation");
    std::fs::create_dir(&prefix)?;
    let source = Package::verify(&fixture(&root.0, A)?)?;
    let _writer = stage::writer(&prefix)?;
    let installed = stage::install(&prefix, &source)?;
    let binary = installed.root.join("grapt.exe");
    let manifest_before = std::fs::read(installed.root.join("release-manifest.json"))?;
    drop(installed);
    std::fs::write(&binary, b"changed after copy")?;
    assert!(stage::install(&prefix, &source).is_err());
    assert_eq!(windows_pointer::read(&prefix)?, None);
    assert_eq!(std::fs::read(binary)?, b"changed after copy");
    assert_eq!(
        std::fs::read(
            prefix
                .join("versions")
                .join(A)
                .join("bin/release-manifest.json")
        )?,
        manifest_before
    );
    Ok(())
}

#[test]
fn revert_transition_preserves_both_held_verified_versions() -> Result {
    let root = Scratch::new()?;
    let prefix = root.0.join("installation");
    std::fs::create_dir(&prefix)?;
    let source_a = Package::verify(&fixture(&root.0, A)?)?;
    let source_b = Package::verify(&fixture(&root.0, B)?)?;
    let _writer = stage::writer(&prefix)?;
    let a = stage::install(&prefix, &source_a)?;
    let b = stage::install(&prefix, &source_b)?;
    let _activation = grapt_service::installation::PrefixGuard::exclusive(&prefix)?;
    for identity in [A, B, A] {
        transition::activate_verified(&prefix, if identity == A { &a } else { &b })?;
        assert_eq!(
            transition::state(&prefix)?
                .ok_or("missing activated state")?
                .current,
            identity
        );
        a.recheck(&a.root)?;
        b.recheck(&b.root)?;
    }
    Ok(())
}

#[test]
fn interrupted_transition_recovers_with_both_verified_versions_held() -> Result {
    for boundary in [
        transition::Boundary::Intent,
        transition::Boundary::Pointer,
        transition::Boundary::State,
    ] {
        let root = Scratch::new()?;
        let prefix = root.0.join("installation");
        std::fs::create_dir(&prefix)?;
        let source_a = Package::verify(&fixture(&root.0, A)?)?;
        let source_b = Package::verify(&fixture(&root.0, B)?)?;
        let _writer = stage::writer(&prefix)?;
        let a = stage::install(&prefix, &source_a)?;
        let b = stage::install(&prefix, &source_b)?;
        let _activation = grapt_service::installation::PrefixGuard::exclusive(&prefix)?;
        transition::activate_verified(&prefix, &a)?;
        let stopped = transition::activate_with(&prefix, B, |seen| {
            if std::mem::discriminant(&seen) == std::mem::discriminant(&boundary) {
                Err("named transition interruption".into())
            } else {
                Ok(())
            }
        });
        assert!(stopped.is_err_and(|message| message == "named transition interruption"));
        transition::recover(&prefix)?;
        let state = transition::state(&prefix)?.ok_or("missing recovered state")?;
        let expected = if matches!(boundary, transition::Boundary::Intent) {
            A
        } else {
            B
        };
        assert_eq!(state.current, expected);
        assert_eq!(windows_pointer::read(&prefix)?.as_deref(), Some(expected));
        assert!(!prefix.join("transition.json").exists());
        a.recheck(&a.root)?;
        b.recheck(&b.root)?;
        // A real revert follows the same transition owner; no version is moved.
        if state.current == B {
            assert_eq!(transition::revert_predecessor(&prefix)?.as_deref(), Some(A));
            transition::activate(&prefix, A)?;
            assert_eq!(
                transition::state(&prefix)?
                    .ok_or("missing reverted state")?
                    .current,
                A
            );
        }
    }
    Ok(())
}

#[test]
fn first_launcher_is_retained_across_update_while_its_file_is_in_use() -> Result {
    let root = Scratch::new()?;
    let prefix = root.0.join("installation");
    std::fs::create_dir(&prefix)?;
    let source_a = Package::verify(&fixture(&root.0, A)?)?;
    let source_b = Package::verify(&fixture(&root.0, B)?)?;
    let _writer = stage::writer(&prefix)?;
    let a = stage::install(&prefix, &source_a)?;
    let b = stage::install(&prefix, &source_b)?;
    let first = super::windows_bootstrap::prepare(&prefix, &a)?;
    first.recheck()?;
    assert_eq!(windows_pointer::read(&prefix)?, None);
    let launcher = prefix.join("bin/grapt.exe");
    let before = std::fs::read(&launcher)?;
    let receipt = std::fs::read(prefix.join("bootstrap.json"))?;
    let _running_launcher = io::regular(&launcher)?;
    let next = super::windows_bootstrap::prepare(&prefix, &b)?;
    next.recheck()?;
    assert_eq!(std::fs::read(&launcher)?, before);
    assert_eq!(std::fs::read(prefix.join("bootstrap.json"))?, receipt);
    assert_eq!(before, std::fs::read(a.root.join("grapt.exe"))?);
    let _activation = grapt_service::installation::PrefixGuard::exclusive(&prefix)?;
    transition::activate_verified(&prefix, &a)?;
    transition::activate_verified(&prefix, &b)?;
    transition::activate_verified(&prefix, &a)?;
    first.recheck()?;
    next.recheck()?;
    Ok(())
}

#[test]
fn interrupted_first_launcher_recovers_only_its_receipted_partial() -> Result {
    let root = Scratch::new()?;
    let prefix = root.0.join("installation");
    std::fs::create_dir(&prefix)?;
    let source_a = Package::verify(&fixture(&root.0, A)?)?;
    let source_b = Package::verify(&fixture(&root.0, B)?)?;
    let _writer = stage::writer(&prefix)?;
    let a = stage::install(&prefix, &source_a)?;
    let b = stage::install(&prefix, &source_b)?;
    let first = super::windows_bootstrap::prepare(&prefix, &a)?;
    drop(first);
    let launcher = prefix.join("bin/grapt.exe");
    let bytes = std::fs::read(&launcher)?;
    std::fs::rename(&launcher, prefix.join("bin/grapt.exe.new"))?;
    let resumed = super::windows_bootstrap::prepare_update(&prefix, &b, false)?;
    let activation = grapt_service::installation::PrefixGuard::exclusive(&prefix)?;
    assert!(
        resumed.finish()?.is_some(),
        "resumed older launcher must report the offline refresh notice"
    );
    drop(activation);
    assert_eq!(std::fs::read(&launcher)?, bytes);
    assert_eq!(windows_pointer::read(&prefix)?, None);
    drop(resumed);
    std::fs::write(&launcher, b"corrupt launcher")?;
    assert!(super::windows_bootstrap::prepare(&prefix, &b).is_err());
    assert_eq!(windows_pointer::read(&prefix)?, None);
    Ok(())
}

#[test]
fn custom_prefix_is_contained_before_any_directory_is_created() -> Result {
    let root = Scratch::new()?;
    let missing = root.0.join("nested/installation");
    let allowed = super::windows_bootstrap::creation_prefix(&missing, &root.0)?;
    assert!(allowed.is_absolute());
    assert!(!missing.exists());
    let outside = root
        .0
        .parent()
        .ok_or("no scratch parent")?
        .join("not-user-installation");
    assert!(super::windows_bootstrap::creation_prefix(&outside, &root.0).is_err());
    assert!(super::windows_bootstrap::creation_prefix(&root.0, &root.0).is_err());
    assert!(
        super::windows_bootstrap::creation_prefix(&root.0.join("../escaped"), &root.0).is_err()
    );
    assert!(super::windows_bootstrap::installed_prefix(&missing.join("bin/grapt.exe"))?.is_none());
    Ok(())
}

#[tokio::test]
#[cfg_attr(windows, expect(clippy::too_many_lines, reason = "legacy"))]
async fn launcher_difference_prints_the_exact_optional_refresh_command() -> Result {
    let root = Scratch::new()?;
    let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) else {
        // The native runner clears user environment values. Run the same
        // witness in a bounded child with a disposable user installation root.
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "installation::windows_stage_tests::launcher_difference_prints_the_exact_optional_refresh_command",
                "--test-threads=1",
                "--nocapture",
            ])
            .env("LOCALAPPDATA", &root.0)
            .env("SystemRoot", system_root())
            .kill_on_drop(true)
            .spawn()?;
        let status =
            match tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await {
                Ok(status) => status?,
                Err(_) => {
                    child.start_kill()?;
                    tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await??;
                    return Err("launcher command witness exceeded its deadline".into());
                }
            };
        if !status.success() {
            return Err("isolated launcher command witness failed".into());
        }
        return Ok(());
    };
    io::directory(&local)?;
    let marker = local.join(format!(
        "grapt-launcher-command-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&marker)?;
    let local_root = Scratch(marker);
    let prefix = local_root.0.join("installation");
    let packages = root.0.join("local $package's Ω");
    std::fs::create_dir(&packages)?;
    let a = fixture(&packages, A)?;
    let b = fixture(&packages, B)?;
    let args = |source: &Path| {
        vec![
            "update".to_owned(),
            "--from".to_owned(),
            source.to_string_lossy().replace('\\', "/"),
            "--prefix".to_owned(),
            prefix.to_string_lossy().into_owned(),
        ]
    };
    let first = super::commands::run(&args(&a)).await?;
    assert!(!first.contains("The fixed launcher"));
    let launcher = prefix.join("bin/grapt.exe");
    let first_bytes = std::fs::read(&launcher)?;
    let update = super::commands::run(&args(&b)).await?;
    println!("measured optional-refresh notice: {update}");
    assert!(
        update.contains("The fixed launcher comes from a build first installed before this one")
    );
    assert!(update.contains(&format!(
        "& '{}' update --from '{}' --prefix '{}' --refresh-launcher",
        b.join("grapt.exe")
            .display()
            .to_string()
            .replace('\'', "''"),
        b.display().to_string().replace('\'', "''"),
        prefix.display().to_string().replace('\'', "''")
    )));
    assert!(update.contains("--refresh-launcher"));
    assert_eq!(std::fs::read(&launcher)?, first_bytes);
    assert_eq!(
        transition::state(&prefix)?
            .ok_or("missing updated state")?
            .current,
        B
    );
    let mut refresh = args(&b);
    refresh.push("--refresh-launcher".into());
    let refreshed = super::commands::run(&refresh).await?;
    assert!(!refreshed.contains("The fixed launcher"));
    assert_eq!(
        std::fs::read(&launcher)?,
        std::fs::read(b.join("grapt.exe"))?
    );
    assert_eq!(
        transition::state(&prefix)?
            .ok_or("missing refreshed state")?
            .current,
        B
    );
    // Retain B's launcher while deliberately selecting older A. B was first
    // activated after A, so the launcher is newer: no refresh is needed and
    // none is offered toward A.
    let newer_launcher = std::fs::read(&launcher)?;
    let downgrade = super::commands::run(&args(&a)).await?;
    println!("measured J07 return to the earlier build: {downgrade}");
    assert!(
        downgrade.contains("No refresh is needed after a deliberate return to an earlier build.")
    );
    assert!(!downgrade.contains("out of date"));
    assert!(!downgrade.contains("--refresh-launcher"), "{downgrade}");
    assert_eq!(std::fs::read(&launcher)?, newer_launcher);
    assert_eq!(
        transition::state(&prefix)?
            .ok_or("missing downgraded state")?
            .current,
        A
    );
    // J07's last step, B again: B's launcher is installed, so nothing differs.
    // (A revert takes this same notice path, but needs a runnable profile
    // checker these fixtures cannot provide.)
    let back = super::commands::run(&args(&b)).await?;
    println!("measured J07 the later build again: {back}");
    assert!(!back.contains("The fixed launcher"), "{back}");
    assert_eq!(
        transition::state(&prefix)?
            .ok_or("missing final state")?
            .current,
        B
    );
    Ok(())
}

#[test]
fn held_launcher_refuses_refresh_without_changing_receipt_or_selection() -> Result {
    let root = Scratch::new()?;
    let prefix = root.0.join("installation");
    std::fs::create_dir(&prefix)?;
    let source_a = Package::verify(&fixture(&root.0, A)?)?;
    let source_b = Package::verify(&fixture(&root.0, B)?)?;
    let _writer = stage::writer(&prefix)?;
    let a = stage::install(&prefix, &source_a)?;
    let b = stage::install(&prefix, &source_b)?;
    super::windows_bootstrap::prepare(&prefix, &a)?.recheck()?;
    let _activation = grapt_service::installation::PrefixGuard::exclusive(&prefix)?;
    transition::activate_verified(&prefix, &a)?;
    let launcher = prefix.join("bin/grapt.exe");
    let before = std::fs::read(&launcher)?;
    let receipt = std::fs::read(prefix.join("bootstrap.json"))?;
    let lease = io::regular(&launcher)?;
    let refresh = super::windows_bootstrap::prepare_update(&prefix, &b, true)?;
    let refusal = refresh.finish();
    assert!(refusal.is_err_and(
        |message| message.contains("fixed launcher cannot be refreshed")
            && message.contains("extracted package's grapt.exe")
    ));
    assert_eq!(std::fs::read(&launcher)?, before);
    assert_eq!(std::fs::read(prefix.join("bootstrap.json"))?, receipt);
    assert_eq!(
        transition::state(&prefix)?
            .ok_or("missing unchanged state")?
            .current,
        A
    );
    drop(lease);
    assert!(refresh.finish()?.is_none());
    assert_eq!(
        std::fs::read(&launcher)?,
        std::fs::read(b.root.join("grapt.exe"))?
    );
    Ok(())
}

#[test]
fn interrupted_refresh_refuses_normal_update_then_explicit_retry_repairs_receipt() -> Result {
    let root = Scratch::new()?;
    let prefix = root.0.join("installation");
    std::fs::create_dir(&prefix)?;
    let source_a = Package::verify(&fixture(&root.0, A)?)?;
    let source_b = Package::verify(&fixture(&root.0, B)?)?;
    let _writer = stage::writer(&prefix)?;
    let a = stage::install(&prefix, &source_a)?;
    let b = stage::install(&prefix, &source_b)?;
    super::windows_bootstrap::prepare(&prefix, &a)?.recheck()?;
    let _activation = grapt_service::installation::PrefixGuard::exclusive(&prefix)?;
    transition::activate_verified(&prefix, &a)?;
    let receipt = std::fs::read(prefix.join("bootstrap.json"))?;
    let refresh = super::windows_bootstrap::prepare_update(&prefix, &b, true)?;
    assert!(refresh
        .interrupt_after_launcher()
        .is_err_and(|message| message == "named launcher publication interruption"));
    drop(refresh);
    assert_eq!(
        std::fs::read(prefix.join("bin/grapt.exe"))?,
        std::fs::read(b.root.join("grapt.exe"))?
    );
    assert_eq!(std::fs::read(prefix.join("bootstrap.json"))?, receipt);
    assert!(super::windows_bootstrap::prepare(&prefix, &b).is_err());
    assert_eq!(
        transition::state(&prefix)?
            .ok_or("missing interrupted state")?
            .current,
        A
    );
    let retry = super::windows_bootstrap::prepare_update(&prefix, &b, true)?;
    assert!(retry.finish()?.is_none());
    super::windows_bootstrap::prepare(&prefix, &b)?.recheck()?;
    transition::activate_verified(&prefix, &b)?;
    assert_eq!(
        transition::state(&prefix)?
            .ok_or("missing repaired state")?
            .current,
        B
    );
    Ok(())
}

#[test]
fn a_version_junction_refuses_without_touching_the_outside_package() -> Result {
    let root = Scratch::new()?;
    let prefix = root.0.join("installation");
    std::fs::create_dir(&prefix)?;
    std::fs::create_dir(prefix.join("versions"))?;
    let outside = root.0.join("outside-version");
    std::fs::create_dir(&outside)?;
    let source = fixture(&root.0, A)?;
    std::fs::rename(&source, outside.join("bin"))?;
    let source = Package::verify(&outside.join("bin"))?;
    let binary = std::fs::read(source.root.join("grapt.exe"))?;
    let manifest = std::fs::read(source.root.join("release-manifest.json"))?;
    let alias = prefix.join("versions").join(A);
    let linked = std::process::Command::new("cmd.exe")
        .args([
            std::ffi::OsStr::new("/C"),
            std::ffi::OsStr::new("mklink"),
            std::ffi::OsStr::new("/J"),
            alias.as_os_str(),
            outside.as_os_str(),
        ])
        .status()?;
    assert!(
        linked.success(),
        "native witness could not create its directory junction"
    );
    let _writer = stage::writer(&prefix)?;
    assert!(
        stage::install(&prefix, &source).is_err(),
        "version junction became an installation target"
    );
    assert_eq!(std::fs::read(source.root.join("grapt.exe"))?, binary);
    assert_eq!(
        std::fs::read(source.root.join("release-manifest.json"))?,
        manifest
    );
    assert_eq!(windows_pointer::read(&prefix)?, None);
    // Remove only this fixture's junction, after checks; never traverse it.
    std::fs::remove_dir(&alias)?;
    Ok(())
}

#[path = "prune_tests.rs"]
mod prune_tests;

#[path = "windows_acl_stage_tests.rs"]
mod acl;
