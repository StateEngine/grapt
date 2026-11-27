//! Synthetic disposable prefixes exercise prune rules, not native package credit.

use super::{fixture, Result, Scratch};
use crate::installation::{package::Package, prune, stage, transition};
use grapt_service::installation::PrefixGuard;
#[cfg(target_os = "linux")]
const BINARY: &str = "bin/grapt";
#[cfg(windows)]
const BINARY: &str = "bin/grapt.exe";

fn installed(root: &Scratch, count: usize) -> Result<(std::path::PathBuf, Vec<String>)> {
    let prefix = root.0.join("installation");
    std::fs::create_dir(&prefix)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700))?;
    }
    let identities: Vec<String> = (1..=count).map(|index| format!("{index:040x}")).collect();
    let _writer = stage::writer(&prefix)?;
    for identity in &identities {
        let source = Package::verify(&fixture(&root.0, identity)?)?;
        let verified = stage::install(&prefix, &source)?;
        let _exclusive = PrefixGuard::exclusive(&prefix)?;
        transition::activate_verified(&prefix, &verified)?;
    }
    Ok((prefix, identities))
}

fn activate_existing(prefix: &std::path::Path, identity: &str) -> Result {
    let _writer = stage::writer(prefix)?;
    let target = Package::verify(&prefix.join("versions").join(identity).join("bin"))?;
    let _exclusive = PrefixGuard::exclusive(prefix)?;
    transition::activate_verified(prefix, &target)?;
    Ok(())
}

#[test]
fn prune_preserves_a_low_sorting_current_package_without_history() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    activate_existing(&prefix, &identities[0])?;
    std::fs::remove_file(prefix.join("history.jsonl"))?;
    let current = prefix.join("versions").join(&identities[0]).join(BINARY);
    let before = std::fs::read(&current)?;
    let state = std::fs::read(prefix.join("state.json"))?;
    prune::run(&prefix)?;
    assert_eq!(std::fs::read(&current)?, before);
    assert_eq!(std::fs::read(prefix.join("state.json"))?, state);
    assert!(!prefix.join("versions").join(&identities[1]).exists());
    Ok(())
}

#[test]
fn prune_preserves_a_low_sorting_previous_package_without_history() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    activate_existing(&prefix, &identities[0])?;
    activate_existing(&prefix, &identities[5])?;
    std::fs::remove_file(prefix.join("history.jsonl"))?;
    let previous = prefix.join("versions").join(&identities[0]).join(BINARY);
    let before = std::fs::read(&previous)?;
    let state = std::fs::read(prefix.join("state.json"))?;
    prune::run(&prefix)?;
    assert_eq!(std::fs::read(&previous)?, before);
    assert_eq!(std::fs::read(prefix.join("state.json"))?, state);
    assert!(!prefix.join("versions").join(&identities[1]).exists());
    Ok(())
}

#[test]
fn prune_keeps_current_and_three_recent_builds_then_is_idempotent() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    let untouched = prefix.join("profile-store");
    std::fs::write(&untouched, b"user history must survive")?;
    let state = std::fs::read(prefix.join("state.json"))?;
    let hint =
        prune::hint(&prefix, &prune::hint_inventory(&prefix)?)?.ok_or("prune hint missing")?;
    assert!(hint.contains("2 old versions"));
    let result = prune::run(&prefix)?;
    for (index, identity) in identities.iter().enumerate() {
        assert_eq!(prefix.join("versions").join(identity).exists(), index >= 2);
    }
    assert!(result.contains(&identities[0]) && result.contains(&identities[1]));
    assert_eq!(std::fs::read(prefix.join("state.json"))?, state);
    assert_eq!(std::fs::read(&untouched)?, b"user history must survive");
    assert!(prune::run(&prefix)?.contains("removed builds: none"));
    assert!(prune::hint(&prefix, &prune::hint_inventory(&prefix)?)?.is_none());
    Ok(())
}

#[test]
fn a_reactivated_old_build_ranks_as_recent_and_previous_survives() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    {
        let _writer = stage::writer(&prefix)?;
        let target = Package::verify(&prefix.join("versions").join(&identities[0]).join("bin"))?;
        let _exclusive = PrefixGuard::exclusive(&prefix)?;
        transition::activate_verified(&prefix, &target)?;
    }
    prune::run(&prefix)?;
    for (index, identity) in identities.iter().enumerate() {
        assert_eq!(
            prefix.join("versions").join(identity).exists(),
            [0, 3, 4, 5].contains(&index)
        );
    }
    let state = transition::state(&prefix)?.ok_or("state missing")?;
    assert_eq!(state.current, identities[0]);
    assert_eq!(state.previous.as_ref(), Some(&identities[5]));
    Ok(())
}

#[test]
fn interrupted_quarantine_with_partial_payload_resumes() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    let source = prefix.join("versions").join(&identities[0]);
    let quarantine = prefix
        .join("versions")
        .join(format!(".prune-{}", identities[0]));
    std::fs::rename(&source, &quarantine)?;
    std::fs::remove_file(quarantine.join(BINARY))?;
    prune::run(&prefix)?;
    assert!(!quarantine.exists());
    assert!(prune::run(&prefix)?.contains("removed builds: none"));
    Ok(())
}

#[test]
fn missing_history_uses_a_named_fallback_but_corrupt_history_stops_all_deletion() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    std::fs::write(prefix.join("history.jsonl"), b"corrupt history")?;
    assert!(prune::run(&prefix).is_err());
    assert!(identities
        .iter()
        .all(|identity| prefix.join("versions").join(identity).exists()));
    std::fs::remove_file(prefix.join("history.jsonl"))?;
    assert!(prune::run(&prefix)?.contains("commit identity as the fallback"));
    Ok(())
}

#[test]
fn an_aliased_inactive_file_refuses_before_other_versions_are_removed() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    let outside = root.0.join("outside-user-file");
    std::fs::write(&outside, b"outside data")?;
    let aliased = prefix.join("versions").join(&identities[0]).join(BINARY);
    std::fs::remove_file(&aliased)?;
    std::fs::hard_link(&outside, &aliased)?;
    let error = prune::run(&prefix)
        .err()
        .ok_or("hard-link alias accepted")?;
    assert!(error.contains("hard-link alias"));
    assert_eq!(std::fs::read(&outside)?, b"outside data");
    assert!(identities
        .iter()
        .all(|identity| prefix.join("versions").join(identity).exists()));
    Ok(())
}

#[test]
fn a_product_shared_guard_excludes_prune_without_partial_deletion() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    let _product = PrefixGuard::shared(&prefix)?;
    assert!(prune::run(&prefix).is_err());
    assert!(identities
        .iter()
        .all(|identity| prefix.join("versions").join(identity).exists()));
    Ok(())
}

#[test]
fn prune_refuses_a_quarantine_naming_the_revert_target() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 4)?;
    let protected = &identities[2];
    let source = prefix.join("versions").join(protected);
    let quarantine = prefix.join("versions").join(format!(".prune-{protected}"));
    std::fs::rename(source, &quarantine)?;
    assert!(prune::run(&prefix).is_err());
    assert!(quarantine.join(BINARY).exists());
    Ok(())
}

#[tokio::test]
async fn prune_source_and_duplicate_options_refuse_before_directory_effects() -> Result {
    for args in [
        vec!["update", "--prune", "--from", "missing"],
        vec!["update", "--prune", "--prune"],
        vec!["revert", "--prune"],
    ] {
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(crate::installation::commands::run(&args).await.is_err());
    }
    Ok(())
}

#[test]
fn a_manifest_removed_quarantine_accepts_only_the_empty_bin_skeleton() -> Result {
    let root = Scratch::new()?;
    let (prefix, _) = installed(&root, 4)?;
    let identity = format!("{:040x}", 100);
    let quarantine = prefix.join("versions").join(format!(".prune-{identity}"));
    std::fs::create_dir_all(quarantine.join("bin"))?;
    std::fs::write(
        quarantine.join("bin/unknown"),
        b"do not delete unknown data",
    )?;
    assert!(prune::run(&prefix).is_err());
    assert!(quarantine.join("bin/unknown").exists());
    std::fs::remove_file(quarantine.join("bin/unknown"))?;
    prune::run(&prefix)?;
    assert!(!quarantine.exists());
    Ok(())
}

#[test]
fn corrupt_remaining_quarantine_bytes_refuse_before_new_retirements() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    let source = prefix.join("versions").join(&identities[0]);
    let quarantine = prefix
        .join("versions")
        .join(format!(".prune-{}", identities[0]));
    std::fs::rename(source, &quarantine)?;
    std::fs::write(quarantine.join(BINARY), b"corrupted payload")?;
    assert!(prune::run(&prefix).is_err());
    assert!(quarantine.join("bin/release-manifest.json").exists());
    assert!(prefix.join("versions").join(&identities[1]).exists());
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_junction_in_an_inactive_build_refuses_without_touching_its_target() -> Result {
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    let original = prefix.join("versions").join(&identities[0]).join("bin");
    let outside = root.0.join("outside-package");
    std::fs::rename(&original, &outside)?;
    let binary = std::fs::read(outside.join("grapt.exe"))?;
    let manifest = std::fs::read(outside.join("release-manifest.json"))?;
    let state = std::fs::read(prefix.join("state.json"))?;
    let history = std::fs::read(prefix.join("history.jsonl"))?;
    let linked = std::process::Command::new("cmd.exe")
        .args([
            std::ffi::OsStr::new("/C"),
            std::ffi::OsStr::new("mklink"),
            std::ffi::OsStr::new("/J"),
            original.as_os_str(),
            outside.as_os_str(),
        ])
        .status()?;
    assert!(
        linked.success(),
        "native prune witness could not create its junction"
    );
    assert!(prune::run(&prefix).is_err());
    assert_eq!(std::fs::read(outside.join("grapt.exe"))?, binary);
    assert_eq!(
        std::fs::read(outside.join("release-manifest.json"))?,
        manifest
    );
    assert_eq!(std::fs::read(prefix.join("state.json"))?, state);
    assert_eq!(std::fs::read(prefix.join("history.jsonl"))?, history);
    assert!(identities
        .iter()
        .all(|identity| prefix.join("versions").join(identity).exists()));
    // Remove only the fixture's junction, then restore its original location.
    std::fs::remove_dir(&original)?;
    std::fs::rename(&outside, &original)?;
    prune::run(&prefix)?;
    assert!(!original.exists());
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_user_held_inactive_reader_refuses_then_release_allows_prune() -> Result {
    use std::os::windows::fs::OpenOptionsExt;
    let root = Scratch::new()?;
    let (prefix, identities) = installed(&root, 6)?;
    let path = prefix.join("versions").join(&identities[0]).join(BINARY);
    let reader = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)?;
    let error = prune::run(&prefix)
        .err()
        .ok_or("held inactive version was removed")?;
    assert!(error.contains("quarantine") || error.contains("open"));
    assert!(path.exists());
    drop(reader);
    prune::run(&prefix)?;
    assert!(!path.exists());
    Ok(())
}
