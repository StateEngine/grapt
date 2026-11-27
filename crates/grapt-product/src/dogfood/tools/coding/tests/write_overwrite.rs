//! Deliberate overwrite through write, asked as fs.overwrite.
use super::*;

/// Every temporary file `write` or `patch` may leave beside `name`.
fn strays(root: &Path, name: &str) -> Result<Vec<String>, String> {
    let prefix = format!(".{name}.grapt-");
    Ok(std::fs::read_dir(root)
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|entry| entry.starts_with(&prefix))
        .collect())
}

#[test]
fn an_existing_file_is_replaced_whole_as_fs_overwrite_and_keeps_its_mode() -> Result<(), String> {
    let scratch = Scratch::new("overwrite")?;
    let root = &scratch.0;
    let notes = root.join("notes.txt");
    std::fs::write(&notes, "old line\n").map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&notes, std::fs::Permissions::from_mode(0o640))
            .map_err(|error| error.to_string())?;
    }
    let value = serde_json::json!({"path":"notes.txt","content":"new line\n"});
    assert_eq!(
        prepare(root, WRITE_TOOL, &input(value.clone())?)?.action,
        "fs.overwrite"
    );
    let receipt = call(root, WRITE_TOOL, value)?;
    assert_eq!(receipt["overwritten"], true);
    assert_eq!(receipt["beforeBytes"], 9);
    let patch = receipt["files"][0]["patch"].as_str().unwrap_or_default();
    assert!(
        patch.contains("-old line") && patch.contains("+new line"),
        "{receipt}"
    );
    assert_eq!(
        std::fs::read(&notes).map_err(|error| error.to_string())?,
        b"new line\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&notes).map_err(|error| error.to_string())?;
        assert_eq!(
            mode.permissions().mode() & 0o777,
            0o640,
            "the file mode changed"
        );
    }
    assert!(strays(root, "notes.txt")?.is_empty());
    let fresh = serde_json::json!({"path":"fresh.txt","content":"x"});
    assert_eq!(
        prepare(root, WRITE_TOOL, &input(fresh)?)?.action,
        "fs.create"
    );
    Ok(())
}

#[test]
fn a_file_changed_or_removed_after_approval_is_left_alone() -> Result<(), String> {
    let scratch = Scratch::new("overwrite-race")?;
    let root = &scratch.0;
    let target = root.join("shared.txt");
    std::fs::write(&target, "v1\n").map_err(|error| error.to_string())?;
    let value = input(serde_json::json!({"path":"shared.txt","content":"mine\n"}))?;
    let EffectTarget::Coding(approved) = prepare(root, WRITE_TOOL, &value)?.target else {
        return Err("wrong prepared effect".into());
    };
    std::fs::write(&target, "v2 by someone else\n").map_err(|error| error.to_string())?;
    let refused = run(root, &approved, &value).err().unwrap_or_default();
    assert!(
        refused.contains("changed after the overwrite was approved"),
        "{refused}"
    );
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"v2 by someone else\n"
    );
    std::fs::remove_file(&target).map_err(|error| error.to_string())?;
    assert!(run(root, &approved, &value).is_err());
    assert!(
        !target.exists(),
        "a removed file was recreated by an overwrite"
    );
    Ok(())
}

#[test]
fn a_directory_or_symlink_target_is_refused() -> Result<(), String> {
    let scratch = Scratch::new("overwrite-kinds")?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("sub")).map_err(|error| error.to_string())?;
    let refused = prepare(
        root,
        WRITE_TOOL,
        &input(serde_json::json!({"path":"sub","content":"x"}))?,
    )
    .err()
    .unwrap_or_default();
    assert!(refused.contains("not a regular file"), "{refused}");
    #[cfg(unix)]
    {
        std::fs::write(root.join("real.txt"), "real\n").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt"))
            .map_err(|error| error.to_string())?;
        let link = serde_json::json!({"path":"link.txt","content":"x"});
        assert!(prepare(root, WRITE_TOOL, &input(link)?).is_err());
        assert_eq!(
            std::fs::read(root.join("real.txt")).map_err(|error| error.to_string())?,
            b"real\n"
        );
    }
    Ok(())
}

/// Unix forces the failure with a rename onto a non-empty directory. Windows
/// cannot use that target (its permissions are the directory's attributes,
/// which a regular temporary file refuses), so there the held-open witness
/// below is the failed-rename witness, with the same no-stray assertion.
#[cfg(unix)]
#[test]
fn a_failed_replacement_leaves_the_original_and_no_temporary_file() -> Result<(), String> {
    let scratch = Scratch::new("overwrite-fail")?;
    let root = &scratch.0;
    // A rename onto a non-empty directory fails on every platform, after the
    // temporary file has been written.
    let blocked = root.join("blocked");
    std::fs::create_dir(&blocked).map_err(|error| error.to_string())?;
    std::fs::write(blocked.join("inside.txt"), "kept\n").map_err(|error| error.to_string())?;
    let refused =
        super::super::patch::atomic_replace(root, &blocked, b"new", "write", "write", &|| Ok(()))
            .err()
            .unwrap_or_default();
    assert!(
        refused.contains("write replacement failed; original retained"),
        "{refused}"
    );
    assert!(
        strays(root, "blocked")?.is_empty(),
        "a temporary file was left behind"
    );
    assert_eq!(
        std::fs::read(blocked.join("inside.txt")).map_err(|error| error.to_string())?,
        b"kept\n"
    );
    Ok(())
}

/// Windows refuses to replace a file another process holds open without
/// delete sharing; the overwrite must say so and leave the file intact.
#[cfg(windows)]
#[test]
fn an_overwrite_of_a_file_held_open_is_refused_and_leaves_it_intact() -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    let scratch = Scratch::new("overwrite-held")?;
    let root = &scratch.0;
    let target = root.join("held.txt");
    std::fs::write(&target, "kept\n").map_err(|error| error.to_string())?;
    let value = input(serde_json::json!({"path":"held.txt","content":"new\n"}))?;
    let EffectTarget::Coding(approved) = prepare(root, WRITE_TOOL, &value)?.target else {
        return Err("wrong prepared effect".into());
    };
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&target)
        .map_err(|error| error.to_string())?;
    let refused = run(root, &approved, &value).err().unwrap_or_default();
    drop(held);
    assert!(refused.contains("original retained"), "{refused}");
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"kept\n"
    );
    assert!(
        strays(root, "held.txt")?.is_empty(),
        "a temporary file was left behind"
    );
    Ok(())
}

#[test]
fn a_read_only_file_is_refused_rather_than_replaced() -> Result<(), String> {
    let scratch = Scratch::new("overwrite-readonly")?;
    let root = &scratch.0;
    let locked = root.join("locked.txt");
    std::fs::write(&locked, "kept\n").map_err(|error| error.to_string())?;
    let mut permissions = std::fs::metadata(&locked)
        .map_err(|error| error.to_string())?
        .permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&locked, permissions).map_err(|error| error.to_string())?;
    let value = serde_json::json!({"path":"locked.txt","content":"new\n"});
    let refused = prepare(root, WRITE_TOOL, &input(value)?)
        .err()
        .unwrap_or_default();
    assert!(refused.contains("write refuses read-only"), "{refused}");
    assert_eq!(
        std::fs::read(&locked).map_err(|error| error.to_string())?,
        b"kept\n"
    );
    let mut permissions = std::fs::metadata(&locked)
        .map_err(|error| error.to_string())?
        .permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&locked, permissions).map_err(|error| error.to_string())?;
    Ok(())
}

/// The snapshot follows a link, so a target swapped for a symlink to the same
/// bytes after approval would match it; the run's own target check refuses it.
#[cfg(unix)]
#[test]
fn a_target_swapped_for_a_symlink_after_approval_is_refused() -> Result<(), String> {
    let scratch = Scratch::new("overwrite-swap")?;
    let root = &scratch.0;
    let target = root.join("swap.txt");
    std::fs::write(&target, "same\n").map_err(|error| error.to_string())?;
    std::fs::write(root.join("elsewhere.txt"), "same\n").map_err(|error| error.to_string())?;
    let value = input(serde_json::json!({"path":"swap.txt","content":"mine\n"}))?;
    let EffectTarget::Coding(approved) = prepare(root, WRITE_TOOL, &value)?.target else {
        return Err("wrong prepared effect".into());
    };
    std::fs::remove_file(&target).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(root.join("elsewhere.txt"), &target)
        .map_err(|error| error.to_string())?;
    assert!(
        run(root, &approved, &value).is_err(),
        "a symlink swap was overwritten"
    );
    assert!(std::fs::symlink_metadata(&target)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_symlink());
    assert_eq!(
        std::fs::read(root.join("elsewhere.txt")).map_err(|error| error.to_string())?,
        b"same\n"
    );
    Ok(())
}

/// The final check runs after staging, just before the rename: with the
/// replacement staged (its temporary file present), a target changed, removed
/// or made read-only is refused by the production check at commit, the change
/// is kept and no temporary file remains. A check that ran only before
/// staging, or no check at all, would replace the file.
#[test]
fn a_target_changed_while_the_replacement_is_staged_is_refused() -> Result<(), String> {
    use std::io::Write as _;
    let scratch = Scratch::new("overwrite-staging")?;
    let root = &scratch.0;
    let target = root.join("staged.txt");
    for (label, interfere) in [("changed", 0_u8), ("removed", 1), ("read-only", 2)] {
        std::fs::write(&target, "v1\n").map_err(|error| error.to_string())?;
        let (before, _) = super::super::snapshot(&target)?;
        super::super::overwrite_still_approved(root, "staged.txt", &target, &before)?;
        let staged = super::super::patch::stage(root, &target, b"mine\n", "write", "write")?;
        assert!(staged.temp().exists(), "{label}: nothing was staged");
        match interfere {
            0 => std::fs::OpenOptions::new()
                .append(true)
                .open(&target)
                .and_then(|mut file| file.write_all(b"other writer\n"))
                .map_err(|error| error.to_string())?,
            1 => std::fs::remove_file(&target).map_err(|error| error.to_string())?,
            _ => {
                let mut permissions = std::fs::metadata(&target)
                    .map_err(|error| error.to_string())?
                    .permissions();
                permissions.set_readonly(true);
                std::fs::set_permissions(&target, permissions)
                    .map_err(|error| error.to_string())?;
            }
        }
        let refused = staged.commit(&|| {
            super::super::overwrite_still_approved(root, "staged.txt", &target, &before).map(drop)
        });
        assert!(
            refused.is_err(),
            "{label}: a target changed while staged was replaced"
        );
        assert!(
            strays(root, "staged.txt")?.is_empty(),
            "{label}: a temporary file was left behind"
        );
        match interfere {
            0 => assert_eq!(
                std::fs::read(&target).map_err(|error| error.to_string())?,
                b"v1\nother writer\n"
            ),
            1 => assert!(!target.exists(), "removed: the file was recreated"),
            _ => {
                assert_eq!(
                    std::fs::read(&target).map_err(|error| error.to_string())?,
                    b"v1\n"
                );
                let mut permissions = std::fs::metadata(&target)
                    .map_err(|error| error.to_string())?
                    .permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                std::fs::set_permissions(&target, permissions)
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    Ok(())
}

#[test]
fn every_overwrite_receipt_names_its_file_even_when_no_text_diff_exists() -> Result<(), String> {
    let scratch = Scratch::new("overwrite-receipt")?;
    let root = &scratch.0;
    std::fs::write(root.join("empty.txt"), "").map_err(|error| error.to_string())?;
    let receipt = call(
        root,
        WRITE_TOOL,
        serde_json::json!({"path":"empty.txt","content":"now\n"}),
    )?;
    assert_eq!(receipt["files"][0]["additions"], 1, "{receipt}");
    assert!(receipt["files"][0]["patch"]
        .as_str()
        .unwrap_or_default()
        .contains("+now"));
    std::fs::write(root.join("bytes.bin"), b"\xff\xfe\n").map_err(|error| error.to_string())?;
    let receipt = call(
        root,
        WRITE_TOOL,
        serde_json::json!({"path":"bytes.bin","content":"text\n"}),
    )?;
    assert_eq!(receipt["files"][0]["patchOmitted"], true, "{receipt}");
    assert_eq!(
        receipt["files"][0]["omittedReason"],
        "binary content replaced; no diff"
    );
    assert!(
        receipt["files"][0]["additions"].is_null() && receipt["files"][0]["deletions"].is_null()
    );
    assert_eq!(receipt["files"][0]["countsAvailable"], false);
    assert_eq!(receipt["files"][0]["relativePath"], "bytes.bin");
    Ok(())
}
