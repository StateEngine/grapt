//! Retained package inventories are literal inputs, independent of new names.
use super::{digest, prefix, Result, Scratch};
use crate::installation::{commands, package, prune, stage, transition};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const TEMPLATE_ID: &str = "1111111111111111111111111111111111111111";
const BINARY: &[u8] = include_bytes!("rename_fixtures/binary.sh");

fn fixture(root: &Path, identity: &str, legacy: bool) -> Result<PathBuf> {
    let (manifest, receipt) = if legacy {
        (
            include_str!("rename_fixtures/secode-manifest.json"),
            include_str!("rename_fixtures/secode-build.json"),
        )
    } else {
        (
            include_str!("rename_fixtures/grapt-manifest.json"),
            include_str!("rename_fixtures/grapt-build.json"),
        )
    };
    let directory = root.join(identity);
    std::fs::create_dir(&directory)?;
    let receipt = receipt.replace(TEMPLATE_ID, identity);
    let mut manifest: Value = serde_json::from_str(&manifest.replace(TEMPLATE_ID, identity))?;
    for (name, entry) in manifest["binaries"]
        .as_object_mut()
        .ok_or("fixture inventory missing")?
    {
        let binary = directory.join(name);
        std::fs::write(&binary, BINARY)?;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
        entry["build_receipt_sha256"] = json!(digest(receipt.as_bytes()));
    }
    std::fs::write(directory.join("build.json"), receipt)?;
    std::fs::write(
        directory.join("release-manifest.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    Ok(directory)
}

#[test]
fn complete_legacy_and_current_inventories_verify_and_inspect() -> Result {
    for legacy in [true, false] {
        let root = Scratch::new()?;
        let source = fixture(&root.0, TEMPLATE_ID, legacy)?;
        let verified = package::Package::verify(&source)?;
        let prefix = prefix(&root.0)?;
        stage::install(&prefix, &verified)?;
        transition::activate(&prefix, TEMPLATE_ID)?;
        let current = transition::verified_current(&prefix)?.ok_or("active package disappeared")?;
        assert_eq!(current.commit, TEMPLATE_ID);
        let paths =
            package::prune_paths(&current.root, TEMPLATE_ID)?.ok_or("prune inventory missing")?;
        let expected = if legacy {
            "secode-server"
        } else {
            "grapt-server"
        };
        assert!(paths.contains(Path::new(expected)));
        assert!(!paths.contains(Path::new(if legacy {
            "grapt-server"
        } else {
            "secode-server"
        })));
    }
    Ok(())
}

#[test]
fn mixed_inventory_refuses_without_staging_or_activation() -> Result {
    for section in ["binaries", "signing", "entry_points"] {
        let root = Scratch::new()?;
        let source = fixture(&root.0, TEMPLATE_ID, true)?;
        let path = source.join("release-manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        if section == "entry_points" {
            manifest[section][0] = json!("grapt-server");
        } else {
            let entries = manifest[section]
                .as_object_mut()
                .ok_or("inventory section missing")?;
            let entry = entries
                .remove("secode-server")
                .ok_or("legacy server missing")?;
            entries.insert("grapt-server".into(), entry);
        }
        std::fs::write(path, serde_json::to_vec(&manifest)?)?;
        let prefix = root.0.join("installation");
        let arguments = vec![
            "update".into(),
            "--from".into(),
            source.to_string_lossy().into_owned(),
            "--prefix".into(),
            prefix.to_string_lossy().into_owned(),
        ];
        let runtime = tokio::runtime::Runtime::new()?;
        assert!(runtime.block_on(commands::run(&arguments)).is_err());
        assert!(!prefix.join("current").exists());
        assert!(!prefix.join("state.json").exists());
        if prefix.join("versions").exists() {
            assert_eq!(std::fs::read_dir(prefix.join("versions"))?.count(), 0);
        }
    }
    Ok(())
}

#[tokio::test]
async fn reverting_to_legacy_uses_its_server_and_keeps_the_public_launcher_working() -> Result {
    let root = Scratch::new()?;
    let prefix = prefix(&root.0)?;
    let newer = "2222222222222222222222222222222222222222";
    for (identity, legacy) in [(TEMPLATE_ID, true), (newer, false)] {
        let source = fixture(&root.0, identity, legacy)?;
        commands::run(&[
            "update".into(),
            "--from".into(),
            source.to_string_lossy().into_owned(),
            "--prefix".into(),
            prefix.to_string_lossy().into_owned(),
        ])
        .await?;
    }
    let profile = root.0.join("profile");
    std::fs::create_dir(&profile)?;
    commands::run(&[
        "revert".into(),
        "--prefix".into(),
        prefix.to_string_lossy().into_owned(),
        "--profile".into(),
        profile.to_string_lossy().into_owned(),
    ])
    .await?;
    assert_eq!(
        transition::state(&prefix)?.ok_or("state missing")?.current,
        TEMPLATE_ID
    );
    let output = std::process::Command::new(prefix.join("test-home/.local/bin/grapt"))
        .args(["retained argument", "--flag"])
        .stdin(std::process::Stdio::null())
        .output()?;
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"retained argument\n--flag\n");
    Ok(())
}

#[test]
fn prune_retires_old_versions_from_both_inventories_and_preserves_current() -> Result {
    let root = Scratch::new()?;
    let prefix = prefix(&root.0)?;
    for index in 1..=6 {
        let identity = format!("{index:040x}");
        let source = fixture(&root.0, &identity, index % 2 == 1)?;
        stage::install(&prefix, &package::Package::verify(&source)?)?;
        transition::activate(&prefix, &identity)?;
    }
    prune::run(&prefix)?;
    for index in 1..=6 {
        assert_eq!(
            prefix
                .join("versions")
                .join(format!("{index:040x}"))
                .exists(),
            index >= 3
        );
    }
    assert!(transition::verified_current(&prefix)?.is_some());
    Ok(())
}

#[tokio::test]
async fn current_package_aliases_preserve_arguments_stdio_and_exit_status() -> Result {
    use std::io::Write;
    let root = Scratch::new()?;
    let source = fixture(&root.0, TEMPLATE_ID, false)?;
    let path = source.join("release-manifest.json");
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    manifest["compat_aliases"] = json!({"secode":"grapt", "secode-server":"grapt-server"});
    manifest["compat_aliases_removal"] = json!("Remove after legacy launcher consumers migrate.");
    std::fs::write(path, serde_json::to_vec(&manifest)?)?;
    let prefix = prefix(&root.0)?;
    commands::run(&[
        "update".into(),
        "--from".into(),
        source.to_string_lossy().into_owned(),
        "--prefix".into(),
        prefix.to_string_lossy().into_owned(),
    ])
    .await?;
    let bin = prefix.join("versions").join(TEMPLATE_ID).join("bin");
    for alias in ["secode", "secode-server"] {
        let mut child = std::process::Command::new(bin.join(alias))
            .args(["argument with spaces", "--flag"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or("alias stdin missing")?
            .write_all(b"retained input\n")?;
        let output = child.wait_with_output()?;
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(
            output.stdout,
            b"argument with spaces\n--flag\nretained input\n"
        );
    }
    Ok(())
}

#[test]
fn unrecognized_aliases_and_wrong_targets_refuse() -> Result {
    for aliases in [
        json!({"secode":"grapt", "secode-server":"grapt-server", "extra":"grapt"}),
        json!({"secode":"grapt-migrate", "secode-server":"grapt-server"}),
    ] {
        let root = Scratch::new()?;
        let source = fixture(&root.0, TEMPLATE_ID, false)?;
        let path = source.join("release-manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        manifest["compat_aliases"] = json!({"secode":"grapt", "secode-server":"grapt-server"});
        manifest["compat_aliases_removal"] =
            json!("Remove after legacy launcher consumers migrate.");
        std::fs::write(&path, serde_json::to_vec(&manifest)?)?;
        assert!(package::Package::verify(&source).is_ok());
        manifest["compat_aliases"] = aliases;
        std::fs::write(path, serde_json::to_vec(&manifest)?)?;
        assert!(package::Package::verify(&source).is_err());
    }
    Ok(())
}
