//! Verified metadata and staging exclusion, without native-process credit.

use std::fs::{File, FileTimes};
use std::time::{Duration, UNIX_EPOCH};

use grapt_service::installation::PrefixGuard;

use super::{fixture, prefix, stage, transition, Package, Result, Scratch, A};

#[test]
fn metadata_changes_refuse_even_when_bytes_and_size_match() -> Result {
    for name in [
        "grapt",
        "release-manifest.json",
        "build.json",
        "plugin-runtime/node",
        "QUICKSTART.md",
    ] {
        let root = Scratch::new()?;
        let package = Package::verify(&fixture(&root.0, A)?)?;
        let path = package.root.join(name);
        let bytes = std::fs::read(&path)?;
        File::options()
            .write(true)
            .open(&path)?
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(7)))?;
        assert_eq!(std::fs::read(path)?, bytes);
        assert!(
            package.recheck(&package.root).is_err(),
            "{name} changed unnoticed"
        );
    }
    Ok(())
}

#[test]
fn replacing_an_input_with_identical_bytes_refuses() -> Result {
    let root = Scratch::new()?;
    let package = Package::verify(&fixture(&root.0, A)?)?;
    let path = package.root.join("grapt");
    let metadata = std::fs::metadata(&path)?;
    let replacement = root.0.join("replacement");
    std::fs::write(&replacement, std::fs::read(&path)?)?;
    std::fs::set_permissions(&replacement, metadata.permissions())?;
    File::options()
        .write(true)
        .open(&replacement)?
        .set_times(FileTimes::new().set_modified(metadata.modified()?))?;
    std::fs::rename(replacement, path)?;
    assert!(package.recheck(&package.root).is_err());
    Ok(())
}

#[test]
fn replacing_the_verified_root_with_an_identical_package_refuses() -> Result {
    let root = Scratch::new()?;
    let package = Package::verify(&fixture(&root.0, A)?)?;
    let retained = root.0.join("retained");
    std::fs::rename(&package.root, &retained)?;
    std::fs::create_dir(&package.root)?;
    // Keep every verified file's identity, so only the root check can detect
    // this replacement. The fixture inventory is fixed and bounded.
    for name in [
        "grapt-server",
        "grapt",
        "grapt-engined",
        "grapt-migrate",
        "grapt-tui",
        "build.json",
        "release-manifest.json",
        "QUICKSTART.md",
        "plugin-runtime",
        "plugin-bridge",
    ] {
        std::fs::rename(retained.join(name), package.root.join(name))?;
    }
    assert!(package.recheck(&package.root).is_err());
    Ok(())
}

#[test]
fn staging_excludes_other_writers_without_excluding_starts() -> Result {
    let root = Scratch::new()?;
    let prefix = prefix(&root.0)?;
    let writer = stage::writer(&prefix)?;
    assert!(
        stage::writer(&prefix).is_err(),
        "a second writer entered staging"
    );
    let start = PrefixGuard::shared(&prefix)?;
    drop(start);
    let activation = PrefixGuard::exclusive(&prefix)?;
    drop(activation);
    drop(writer);
    assert!(prefix.join(".staging/lock").is_file());
    let retry = stage::writer(&prefix)?;
    drop(retry);
    Ok(())
}

#[test]
fn changed_active_snapshot_refuses_without_rewriting_records() -> Result {
    let root = Scratch::new()?;
    let prefix = prefix(&root.0)?;
    stage::install(&prefix, &Package::verify(&fixture(&root.0, A)?)?)?;
    transition::activate(&prefix, A)?;
    let verified = transition::verified_current(&prefix)?;
    let state = std::fs::read(prefix.join("state.json"))?;
    let history = std::fs::read(prefix.join("history.jsonl"))?;
    std::fs::write(
        prefix.join("versions").join(A).join("bin/grapt"),
        b"changed",
    )?;
    assert!(transition::recover_verified(&prefix, verified.as_ref()).is_err());
    assert_eq!(std::fs::read(prefix.join("state.json"))?, state);
    assert_eq!(std::fs::read(prefix.join("history.jsonl"))?, history);
    assert!(!prefix.join("transition.json").exists());
    Ok(())
}

#[tokio::test]
async fn revert_recovers_a_first_install_before_refusing_no_predecessor() -> Result {
    for stop in [transition::Boundary::Intent, transition::Boundary::Pointer] {
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
        let profile = root.0.join("absent-profile");
        let refusal = super::super::commands::run(&[
            "revert".into(),
            "--prefix".into(),
            prefix.to_string_lossy().into_owned(),
            "--profile".into(),
            profile.to_string_lossy().into_owned(),
        ])
        .await
        .err()
        .ok_or("revert invented a predecessor")?;
        assert!(
            refusal.contains("no current build") || refusal.contains("no previous build"),
            "{refusal}"
        );
        assert!(!prefix.join("transition.json").exists());
        assert!(!profile.exists());
        let recovered = transition::state(&prefix)?;
        if matches!(stop, transition::Boundary::Intent) {
            assert!(recovered.is_none());
        } else {
            let recovered = recovered.ok_or("activated first install was lost")?;
            assert_eq!(recovered.current, A);
            assert!(recovered.previous.is_none());
        }
    }
    Ok(())
}
