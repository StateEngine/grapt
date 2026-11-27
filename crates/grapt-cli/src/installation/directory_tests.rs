//! Existing-directory corruption refuses before an installer effect.

use super::{fixture, prefix, stage, transition, Package, Result, Scratch, A, B};
use crate::installation::io;

#[test]
fn linked_existing_version_and_bin_refuse_before_reuse() -> Result {
    for linked in ["version", "bin"] {
        let root = Scratch::new()?;
        let package = Package::verify(&fixture(&root.0, A)?)?;
        let prefix = prefix(&root.0)?;
        let outside = root.0.join("outside-version");
        std::fs::create_dir(&outside)?;
        std::os::unix::fs::symlink(&package.root, outside.join("bin"))?;
        let versions = prefix.join("versions");
        std::fs::create_dir(&versions)?;
        if linked == "version" {
            // The external bin must itself be real, so the version-root check
            // is the discriminating refusal rather than a nested bin link.
            std::fs::remove_file(outside.join("bin"))?;
            std::fs::rename(&package.root, outside.join("bin"))?;
            std::os::unix::fs::symlink(&outside, versions.join(A))?;
            let package = Package::verify(&outside.join("bin"))?;
            assert!(
                stage::install(&prefix, &package).is_err(),
                "linked version was accepted"
            );
        } else {
            std::fs::create_dir(versions.join(A))?;
            std::os::unix::fs::symlink(&package.root, versions.join(A).join("bin"))?;
            assert!(
                stage::install(&prefix, &package).is_err(),
                "linked bin was accepted"
            );
        }
        assert!(!prefix.join("current").exists());
        assert!(!prefix.join("state.json").exists());
        assert!(!prefix.join("transition.json").exists());
    }
    Ok(())
}

#[test]
fn linked_versions_and_partial_refuse_without_removing_outside_bytes() -> Result {
    for linked in ["versions", "partial"] {
        let root = Scratch::new()?;
        let package = Package::verify(&fixture(&root.0, A)?)?;
        let prefix = prefix(&root.0)?;
        let outside = root.0.join("outside");
        std::fs::create_dir(&outside)?;
        std::fs::write(outside.join("keep"), b"untouched")?;
        if linked == "versions" {
            std::os::unix::fs::symlink(&outside, prefix.join("versions"))?;
        } else {
            std::fs::create_dir(prefix.join("versions"))?;
            std::os::unix::fs::symlink(&outside, prefix.join("versions/.partial"))?;
        }
        assert!(
            stage::install(&prefix, &package).is_err(),
            "linked staging route was accepted"
        );
        assert_eq!(std::fs::read(outside.join("keep"))?, b"untouched");
        assert!(!prefix.join("current").exists());
    }
    Ok(())
}

#[test]
fn substituted_package_parent_is_rechecked_when_inputs_are_copied() -> Result {
    let root = Scratch::new()?;
    let package = Package::verify(&fixture(&root.0, A)?)?;
    let prefix = prefix(&root.0)?;
    let moved = root.0.join("moved-package");
    std::fs::rename(&package.root, &moved)?;
    std::os::unix::fs::symlink(&moved, &package.root)?;
    assert!(
        stage::install(&prefix, &package).is_err(),
        "linked input parent was accepted"
    );
    assert!(!prefix.join("versions").join(A).exists());
    assert!(!prefix.join("current").exists());
    assert!(moved.join("grapt").is_file());
    Ok(())
}

#[test]
fn linked_pointer_targets_refuse_and_preserve_transition_records() -> Result {
    for linked in ["version", "bin"] {
        let root = Scratch::new()?;
        let prefix = prefix(&root.0)?;
        for identity in [A, B] {
            stage::install(&prefix, &Package::verify(&fixture(&root.0, identity)?)?)?;
        }
        transition::activate(&prefix, A)?;
        let failed = transition::activate_with(&prefix, B, |boundary| {
            if matches!(boundary, transition::Boundary::Pointer) {
                Err("injected pointer cut".into())
            } else {
                Ok(())
            }
        });
        assert!(failed.is_err());
        let original = if linked == "version" {
            prefix.join("versions").join(B)
        } else {
            prefix.join("versions").join(B).join("bin")
        };
        let moved = root.0.join("moved-target");
        std::fs::rename(&original, &moved)?;
        std::os::unix::fs::symlink(&moved, &original)?;
        let state = std::fs::read(prefix.join("state.json"))?;
        let intent = std::fs::read(prefix.join("transition.json"))?;
        let history = std::fs::read(prefix.join("history.jsonl"))?;
        assert!(
            transition::recover(&prefix).is_err(),
            "linked pointer target was accepted"
        );
        assert_eq!(std::fs::read(prefix.join("state.json"))?, state);
        assert_eq!(std::fs::read(prefix.join("transition.json"))?, intent);
        assert_eq!(std::fs::read(prefix.join("history.jsonl"))?, history);
        assert_eq!(
            std::fs::read_link(prefix.join("current"))?,
            std::path::Path::new("versions").join(B)
        );
    }
    Ok(())
}

#[test]
fn changed_installed_bytes_refuse_before_activation() -> Result {
    let root = Scratch::new()?;
    let prefix = prefix(&root.0)?;
    for identity in [A, B] {
        stage::install(&prefix, &Package::verify(&fixture(&root.0, identity)?)?)?;
    }
    transition::activate(&prefix, A)?;
    let verified = Package::verify(&prefix.join("versions").join(B).join("bin"))?;
    std::fs::write(
        prefix.join("versions").join(B).join("bin/grapt"),
        b"corrupt",
    )?;
    let state = std::fs::read(prefix.join("state.json"))?;
    assert!(
        transition::activate_verified(&prefix, &verified).is_err(),
        "corrupt installed bytes were accepted"
    );
    assert_eq!(transition::pointer(&prefix)?.as_deref(), Some(A));
    assert_eq!(std::fs::read(prefix.join("state.json"))?, state);
    assert!(!prefix.join("transition.json").exists());
    Ok(())
}

#[test]
fn linked_parent_refuses_record_creation_and_directory_sync() -> Result {
    let root = Scratch::new()?;
    let outside = root.0.join("outside");
    std::fs::create_dir(&outside)?;
    let linked = root.0.join("linked");
    std::os::unix::fs::symlink(&outside, &linked)?;
    assert!(io::write_new(&linked.join("state.json"), b"{}").is_err());
    assert!(io::sync_directory(&linked).is_err());
    assert!(!outside.join("state.json").exists());
    Ok(())
}

#[test]
fn partial_bin_link_is_unlinked_without_following_outside_contents() -> Result {
    let root = Scratch::new()?;
    let prefix = prefix(&root.0)?;
    let outside = root.0.join("outside-partial");
    std::fs::create_dir(&outside)?;
    std::fs::write(outside.join("keep"), b"untouched")?;
    std::fs::create_dir_all(prefix.join("versions/.partial"))?;
    std::os::unix::fs::symlink(&outside, prefix.join("versions/.partial/bin"))?;
    let package = Package::verify(&fixture(&root.0, A)?)?;
    stage::install(&prefix, &package)?;
    assert_eq!(std::fs::read(outside.join("keep"))?, b"untouched");
    assert!(!prefix.join("versions/.partial").exists());
    let installed = prefix.join("versions").join(A).join("bin");
    assert!(!std::fs::symlink_metadata(&installed)?
        .file_type()
        .is_symlink());
    assert_eq!(Package::verify(&installed)?.commit, A);
    Ok(())
}
