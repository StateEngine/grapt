//! Native R packages keep the existing install, revert and prune boundaries.
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{fixture as current_fixture, stage, transition, Package, Result, Scratch, A, B};

#[cfg(target_os = "linux")]
const RECEIPT: &str = "build.json";
#[cfg(windows)]
const RECEIPT: &str = "build-receipt.json";
#[cfg(target_os = "linux")]
const OLD: &str = "secode-tui-r"; // grapt-rename: keep
#[cfg(windows)]
const OLD: &str = "secode-tui-r.exe"; // grapt-rename: keep
#[cfg(target_os = "linux")]
const NATIVE: &str = "grapt-tui";
#[cfg(windows)]
const NATIVE: &str = "grapt-tui.exe";

fn native_fixture(root: &Path, identity: &str) -> Result<PathBuf> {
    current_fixture(root, identity)
}

fn fixture(root: &Path, identity: &str) -> Result<PathBuf> {
    let package = current_fixture(root, identity)?;
    let legacy = |text: &str| {
        text.replace("grapt", "secode")
            .replace("secode-tui", "secode-tui-r")
    };
    for entry in std::fs::read_dir(&package)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "fixture name is not UTF-8")?;
        if name.starts_with("grapt") {
            std::fs::rename(entry.path(), package.join(legacy(&name)))?;
        }
    }
    let receipt = legacy(&std::fs::read_to_string(package.join(RECEIPT))?);
    std::fs::write(package.join(RECEIPT), &receipt)?;
    let hash = format!("{:x}", Sha256::digest(receipt.as_bytes()));
    let text = legacy(&std::fs::read_to_string(
        package.join("release-manifest.json"),
    )?);
    let mut manifest: Value = serde_json::from_str(&text)?;
    for binary in manifest["binaries"]
        .as_object_mut()
        .ok_or("fixture has no binaries")?
        .values_mut()
    {
        binary["build_receipt_sha256"] = json!(hash);
    }
    std::fs::write(
        package.join("release-manifest.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    Ok(package)
}

fn installation_prefix(root: &Path) -> Result<PathBuf> {
    let prefix = root.join("installation");
    std::fs::create_dir(&prefix)?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(prefix)
}

#[test]
fn native_frontend_upgrade_and_revert_preserve_verified_legacy_packages() -> Result {
    let root = Scratch::new()?;
    let prefix = installation_prefix(&root.0)?;
    let old = Package::verify(&fixture(&root.0, A)?)?;
    let native = Package::verify(&native_fixture(&root.0, B)?)?;
    let _writer = stage::writer(&prefix)?;
    let old = stage::install(&prefix, &old)?;
    let native = stage::install(&prefix, &native)?;
    let _activation = grapt_service::installation::PrefixGuard::exclusive(&prefix)?;
    for package in [&old, &native, &old] {
        transition::activate_verified(&prefix, package)?;
        assert_eq!(
            transition::state(&prefix)?.ok_or("missing state")?.current,
            package.commit
        );
        Package::verify(&old.root)?;
        Package::verify(&native.root)?;
    }
    assert!(old.root.join(OLD).is_file());
    assert!(native.root.join(NATIVE).is_file());
    assert!(!native.root.join(OLD).exists());
    Ok(())
}

#[test]
fn interrupted_native_frontend_upgrade_keeps_recovery_and_revert() -> Result {
    for boundary in [
        transition::Boundary::Intent,
        transition::Boundary::Pointer,
        transition::Boundary::State,
    ] {
        let root = Scratch::new()?;
        let prefix = installation_prefix(&root.0)?;
        let _writer = stage::writer(&prefix)?;
        stage::install(&prefix, &Package::verify(&fixture(&root.0, A)?)?)?;
        stage::install(&prefix, &Package::verify(&native_fixture(&root.0, B)?)?)?;
        let _activation = grapt_service::installation::PrefixGuard::exclusive(&prefix)?;
        transition::activate(&prefix, A)?;
        let stopped = transition::activate_with(&prefix, B, |seen| {
            if std::mem::discriminant(&seen) == std::mem::discriminant(&boundary) {
                Err("interrupted frontend upgrade".into())
            } else {
                Ok(())
            }
        });
        assert!(stopped.is_err_and(|message| message == "interrupted frontend upgrade"));
        transition::recover(&prefix)?;
        let expected = if matches!(boundary, transition::Boundary::Intent) {
            A
        } else {
            B
        };
        assert_eq!(
            transition::state(&prefix)?.ok_or("missing state")?.current,
            expected
        );
        transition::activate(&prefix, A)?;
        assert!(!prefix.join("transition.json").exists());
    }
    Ok(())
}

#[test]
fn native_frontend_prune_inventory_remains_bounded_to_each_package() -> Result {
    let root = Scratch::new()?;
    for (package, identity, frontend) in [
        (fixture(&root.0, A)?, A, OLD),
        (native_fixture(&root.0, B)?, B, NATIVE),
    ] {
        let paths = super::super::package::prune_paths(&package, identity)?
            .ok_or("missing prune inventory")?;
        assert!(paths.contains(Path::new(frontend)));
        assert!(!paths.contains(Path::new(if frontend == OLD { NATIVE } else { OLD })));
        std::fs::remove_file(package.join(frontend))?;
        assert!(super::super::package::prune_paths(&package, identity)?.is_some());
    }
    Ok(())
}

#[test]
fn mixed_frontend_inventory_refuses_before_activation() -> Result {
    let root = Scratch::new()?;
    let package = native_fixture(&root.0, A)?;
    let path = package.join("release-manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    manifest["binaries"][OLD] = manifest["binaries"][NATIVE].clone();
    std::fs::write(path, serde_json::to_vec(&manifest)?)?;
    assert!(Package::verify(&package).is_err_and(|message| {
        message == "package does not declare the complete shipped binary set"
    }));
    assert!(!root.0.join("installation/current").exists());
    Ok(())
}
