use super::super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Result<Self, String> {
        let path = std::env::temp_dir().join(format!(
            "grapt-text-patch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).map_err(|e| e.to_string())?;
        Ok(Self(
            std::fs::canonicalize(path).map_err(|e| e.to_string())?,
        ))
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

fn input(patch: &str) -> Result<BoundedToolInput, String> {
    BoundedToolInput::new(serde_json::json!({"input":patch}).to_string().into_bytes())
        .map_err(|e| e.to_string())
}

#[test]
fn text_hunks_use_the_governed_multi_file_patch_plan() -> Result<(), String> {
    let root = Root::new()?;
    for (name, bytes) in [
        ("update.txt", "old\r\nkeep\r\n"),
        ("delete.txt", "delete\n"),
        ("move.txt", "old\n"),
    ] {
        std::fs::write(root.0.join(name), bytes).map_err(|e| e.to_string())?;
    }
    let input=input("*** Begin Patch\n*** Add File: added.txt\n+new\n*** Update File: update.txt\n@@\n-old\n+changed\n keep\n*** Delete File: delete.txt\n*** Update File: move.txt\n*** Move to: moved.txt\n@@\n-old\n+moved and changed\n*** End Patch")?;
    let prepared = prepare(&root.0, &input)?;
    assert_eq!(prepared.action, PATCH_TOOL);
    let EffectTarget::Coding(CodingTarget::Patch(plan)) = prepared.target else {
        return Err("text patch escaped the governed patch executor".into());
    };
    let actions = plan
        .consequences()
        .into_iter()
        .map(|c| c.action)
        .collect::<Vec<_>>();
    assert!(actions.contains(&"fs.move".into()) && actions.contains(&"fs.modify".into()));
    let output = super::super::super::run(&root.0, &plan, &input)?;
    let output: serde_json::Value = serde_json::from_slice(&output).map_err(|e| e.to_string())?;
    assert_eq!(output["complete"], true);
    for (name, bytes) in [
        ("added.txt", "new\n"),
        ("update.txt", "changed\r\nkeep\r\n"),
        ("moved.txt", "moved and changed\n"),
    ] {
        assert_eq!(
            std::fs::read(root.0.join(name)).map_err(|e| e.to_string())?,
            bytes.as_bytes()
        );
    }
    assert!(!root.0.join("delete.txt").exists() && !root.0.join("move.txt").exists());
    assert!(
        super::super::super::run(&root.0, &plan, &input).is_err(),
        "a stale retry must not duplicate effects"
    );
    Ok(())
}

#[test]
fn permission_wait_rechecks_every_source_before_the_first_effect() -> Result<(), String> {
    let root = Root::new()?;
    std::fs::write(root.0.join("source"), "old\n").map_err(|e| e.to_string())?;
    let input=input("*** Begin Patch\n*** Add File: must-not-exist\n+new\n*** Update File: source\n@@\n-old\n+new\n*** End Patch")?;
    let expected = plan(&root.0, &input)?;
    std::fs::write(root.0.join("source"), "old\nconcurrent edit\n").map_err(|e| e.to_string())?;
    let result = super::super::super::run(&root.0, &expected, &input);
    assert!(result.is_err_and(|error| error.contains("changed after authorization")));
    assert!(!root.0.join("must-not-exist").exists());
    assert_eq!(
        std::fs::read_to_string(root.0.join("source")).map_err(|e| e.to_string())?,
        "old\nconcurrent edit\n"
    );
    Ok(())
}

#[test]
fn escapes_and_invalid_context_refuse_without_writing() -> Result<(), String> {
    let root = Root::new()?;
    std::fs::write(root.0.join("source"), "original\n").map_err(|e| e.to_string())?;
    for body in [
        "*** Add File: ../outside\n+new",
        "*** Update File: source\n@@\n-not found\n+new",
    ] {
        let input = input(&format!("*** Begin Patch\n{body}\n*** End Patch"))?;
        assert!(plan(&root.0, &input).is_err());
    }
    assert_eq!(
        std::fs::read_to_string(root.0.join("source")).map_err(|e| e.to_string())?,
        "original\n"
    );
    assert!(!root.0.join("same").exists());
    Ok(())
}

#[test]
fn additions_overwrite_create_parents_and_later_hunks_observe_earlier_results() -> Result<(), String>
{
    let root = Root::new()?;
    std::fs::write(root.0.join("occupied"), "old destination\n").map_err(|e| e.to_string())?;
    let input=input("*** Begin Patch\n*** Add File: same\n+first\n*** Add File: same\n+second\n*** Update File: same\n*** Move to: occupied\n@@\n-second\n+moved\n*** Add File: deep/new/file\n+leaf\n*** Update File: deep/new/file\n@@\n-leaf\n+leaf two\n*** End Patch")?;
    let expected = plan(&root.0, &input)?;
    let effects = expected.consequences();
    assert!(effects
        .iter()
        .any(|item| item.action == "fs.create" && item.resource.ends_with("deep")));
    assert!(effects
        .iter()
        .any(|item| item.action == "fs.overwrite" && item.resource.ends_with("occupied")));
    super::super::super::run(&root.0, &expected, &input)?;
    assert_eq!(
        std::fs::read_to_string(root.0.join("occupied")).map_err(|e| e.to_string())?,
        "moved\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.0.join("deep/new/file")).map_err(|e| e.to_string())?,
        "leaf two\n"
    );
    assert!(!root.0.join("same").exists());
    Ok(())
}

#[test]
fn a_deleted_file_can_become_a_parent_directory_in_the_same_patch() -> Result<(), String> {
    let root = Root::new()?;
    std::fs::write(root.0.join("parent"), "old file\n").map_err(|e| e.to_string())?;
    let input=input("*** Begin Patch\n*** Delete File: parent\n*** Add File: parent/child\n+new file\n*** End Patch")?;
    let expected = plan(&root.0, &input)?;
    super::super::super::run(&root.0, &expected, &input)?;
    assert_eq!(
        std::fs::read_to_string(root.0.join("parent/child")).map_err(|e| e.to_string())?,
        "new file\n"
    );
    Ok(())
}

#[test]
fn protected_paths_and_file_parents_refuse_before_any_effect() -> Result<(), String> {
    let root = Root::new()?;
    std::fs::write(root.0.join("AGENTS.md"), "original authority\n").map_err(|e| e.to_string())?;
    for requested in [
        ".git/config",
        ".grapt/grapt.jsonc",
        "nested/.grapt/provider.credential",
        ".stateengine/config",
        ".codex/config",
        "AGENTS.md",
        "AGENTS.override.md",
        "nested/AGENTS.md",
        "nested/AGENTS.override.md",
        "CLAUDE.md",
    ] {
        let input=input(&format!("*** Begin Patch\n*** Add File: safe-first\n+safe\n*** Add File: {requested}\n+bad\n*** End Patch"))?;
        assert!(
            plan(&root.0, &input).is_err_and(|error| error.contains("protected control metadata"))
        );
        assert!(!root.0.join("safe-first").exists());
    }
    let input=input("*** Begin Patch\n*** Add File: file-parent\n+file\n*** Add File: file-parent/child\n+bad\n*** End Patch")?;
    assert!(plan(&root.0, &input).is_err_and(|error| error.contains("parent is a file")));
    assert!(!root.0.join("file-parent").exists());
    assert_eq!(
        std::fs::read_to_string(root.0.join("AGENTS.md")).map_err(|e| e.to_string())?,
        "original authority\n"
    );
    Ok(())
}

#[test]
fn a_parent_or_overwrite_changed_during_permission_refuses_the_whole_patch() -> Result<(), String> {
    let root = Root::new()?;
    let input = input("*** Begin Patch\n*** Add File: deep/file\n+new\n*** End Patch")?;
    let expected = plan(&root.0, &input)?;
    std::fs::create_dir(root.0.join("deep")).map_err(|e| e.to_string())?;
    assert!(super::super::super::run(&root.0, &expected, &input)
        .is_err_and(|e| e.contains("changed after authorization")));
    assert!(!root.0.join("deep/file").exists());
    Ok(())
}

#[test]
fn a_read_only_overwrite_is_rejected_during_preflight() -> Result<(), String> {
    let root = Root::new()?;
    let target = root.0.join("protected-file");
    std::fs::write(&target, "old\n").map_err(|e| e.to_string())?;
    let original = std::fs::metadata(&target)
        .map_err(|e| e.to_string())?
        .permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    std::fs::set_permissions(&target, readonly).map_err(|e| e.to_string())?;
    let input=input("*** Begin Patch\n*** Add File: safe-first\n+safe\n*** Add File: protected-file\n+new\n*** End Patch")?;
    let result = plan(&root.0, &input);
    std::fs::set_permissions(&target, original).map_err(|e| e.to_string())?;
    assert!(result.is_err_and(|e| e.contains("read-only")));
    assert!(!root.0.join("safe-first").exists());
    assert_eq!(
        std::fs::read_to_string(target).map_err(|e| e.to_string())?,
        "old\n"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn linked_parents_and_sources_cannot_redirect_a_patch() -> Result<(), String> {
    let root = Root::new()?;
    let outside = Root::new()?;
    std::fs::write(outside.0.join("file"), "outside\n").map_err(|e| e.to_string())?;
    std::os::unix::fs::symlink(&outside.0, root.0.join("link")).map_err(|e| e.to_string())?;
    for body in [
        "*** Add File: link/new\n+bad",
        "*** Update File: link/file\n@@\n-outside\n+bad",
    ] {
        let input = input(&format!("*** Begin Patch\n{body}\n*** End Patch"))?;
        assert!(plan(&root.0, &input).is_err());
    }
    assert!(!outside.0.join("new").exists());
    assert_eq!(
        std::fs::read_to_string(outside.0.join("file")).map_err(|e| e.to_string())?,
        "outside\n"
    );
    Ok(())
}

#[test]
fn missing_parent_count_is_bounded_before_filesystem_effects() -> Result<(), String> {
    let root = Root::new()?;
    let path = format!("{}file", "d/".repeat(65));
    let input = input(&format!(
        "*** Begin Patch\n*** Add File: {path}\n+new\n*** End Patch"
    ))?;
    assert!(plan(&root.0, &input).is_err_and(|error| error.contains("64 new parent directories")));
    assert!(!root.0.join("d").exists());
    Ok(())
}

#[test]
fn a_failed_move_removal_reports_the_written_destination_and_keeps_the_source() -> Result<(), String>
{
    let root = Root::new()?;
    std::fs::write(root.0.join("from"), "old\n").map_err(|e| e.to_string())?;
    let input = input(
        "*** Begin Patch\n*** Update File: from\n*** Move to: to\n@@\n-old\n+new\n*** End Patch",
    )?;
    let expected = plan(&root.0, &input)?;
    let step = expected
        .0
        .iter()
        .find(|step| matches!(step, PatchStep::TextPatchMove { .. }))
        .ok_or("move missing")?;
    let mut progress = Progress::default();
    let result = super::super::filesystem::move_effect(&root.0, step, 0, &mut progress, |_| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "injected remove refusal",
        ))
    });
    assert!(result.is_err_and(
        |error| error.contains("was written") && error.contains("could not be removed")
    ));
    let PatchStep::TextPatchMove { to, .. } = step else {
        return Err("move missing".into());
    };
    assert_eq!(
        progress.written,
        std::slice::from_ref(to),
        "the written destination is reported"
    );
    assert!(!progress.uncertain);
    assert_eq!(
        std::fs::read_to_string(root.0.join("from")).map_err(|e| e.to_string())?,
        "old\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.0.join("to")).map_err(|e| e.to_string())?,
        "new\n"
    );
    Ok(())
}

#[cfg(windows)]
#[test]
fn windows_case_aliases_share_one_virtual_file_and_directory() -> Result<(), String> {
    let root = Root::new()?;
    let input=input("*** Begin Patch\n*** Add File: Nested/File\n+first\n*** Update File: nested/file\n@@\n-first\n+second\n*** End Patch")?;
    let expected = plan(&root.0, &input)?;
    super::super::super::run(&root.0, &expected, &input)?;
    assert_eq!(
        std::fs::read_to_string(root.0.join("NESTED/FILE")).map_err(|e| e.to_string())?,
        "second\n"
    );
    Ok(())
}

#[test]
fn additions_require_overwrite_authority_but_context_edits_keep_modify_authority(
) -> Result<(), String> {
    use grapt_permission::preset::{decide, Effect, Layer, Preset};
    let root = Root::new()?;
    std::fs::write(root.0.join("occupied"), "old\n").map_err(|error| error.to_string())?;
    let add = plan(
        &root.0,
        &input("*** Begin Patch\n*** Add File: occupied\n+replacement\n*** End Patch")?,
    )?;
    let update = plan(
        &root.0,
        &input(
            "*** Begin Patch\n*** Update File: occupied\n@@\n-old\n+replacement\n*** End Patch",
        )?,
    )?;
    assert_eq!(add.consequences()[0].action, "fs.overwrite");
    assert_eq!(update.consequences()[0].action, "fs.modify");
    assert_eq!(
        decide(
            &add.consequences()[0].action,
            &["occupied"],
            &[Layer::owner(), Layer::preset(Preset::Author)]
        )
        .effect,
        Effect::Ask
    );
    Ok(())
}

#[test]
fn undo_capture_folds_repeated_hunks_and_moves_into_one_postimage_per_file() -> Result<(), String> {
    let root = Root::new()?;
    std::fs::write(root.0.join("source.txt"), "one\n").map_err(|e| e.to_string())?;
    let input = input("*** Begin Patch\n*** Update File: source.txt\n@@\n-one\n+two\n*** Update File: source.txt\n*** Move to: dir/moved.txt\n@@\n-two\n+three\n*** Add File: source.txt\n+again\n*** End Patch")?;
    let EffectTarget::Coding(CodingTarget::Patch(plan)) = prepare(&root.0, &input)?.target else {
        return Err("text patch escaped the governed patch executor".into());
    };
    let planned = planned_files(&root.0, &plan, &input)?;
    let (source, moved) = (Path::new("source.txt"), Path::new("dir/moved.txt"));
    assert_eq!(planned.paths(), [moved, source]);
    // Steps: update, make dir, move, add.
    let after_update = planned.postimages(1, &[]);
    assert_eq!(after_update.get(source), Some(&Some(&b"two\n"[..])));
    assert_eq!(after_update.get(moved), None);
    let after_move = planned.postimages(3, &[]);
    assert_eq!(after_move.get(source), Some(&None));
    assert_eq!(after_move.get(moved), Some(&Some(&b"three\n"[..])));
    assert_eq!(
        planned.postimages(usize::MAX, &[]).get(source),
        Some(&Some(&b"again\n"[..]))
    );
    let execution = run_observed(&root.0, &plan, &input, None)?;
    assert!(execution.output.is_ok());
    assert_eq!(execution.applied_steps, 4);
    assert_eq!(
        execution.applied,
        [moved, source],
        "directories and repeats"
    );
    assert!(execution.applied_modes.contains_key(source));
    assert!(execution.applied_modes.contains_key(moved));
    Ok(())
}

#[test]
fn completed_patch_receipt_preserves_offsets_overwrite_and_delete_preimages() -> Result<(), String>
{
    let root = Root::new()?;
    let prefix: String = (0..15).map(|n| format!("keep_{n}\n")).collect();
    for (path, contents) in [
        ("update.rs", format!("{prefix}before\ntail\n")),
        ("overwrite.txt", "old1\nold2\nold3\n".into()),
        ("delete.txt", "one\ntwo\n".into()),
    ] {
        std::fs::write(root.0.join(path), contents).map_err(|e| e.to_string())?;
    }
    let input = input("*** Begin Patch\n*** Update File: update.rs\n@@ keep_14\n-before\n+after\n*** Add File: overwrite.txt\n+replacement\n*** Delete File: delete.txt\n*** End Patch")?;
    let plan = plan(&root.0, &input)?;
    let output = super::super::super::run(&root.0, &plan, &input)?;
    let value: serde_json::Value = serde_json::from_slice(&output).map_err(|e| e.to_string())?;
    let files = value["files"].as_array().ok_or("display receipt missing")?;
    let file = |path| {
        files
            .iter()
            .find(|f| f["relativePath"] == path)
            .ok_or("file receipt missing".to_owned())
    };
    assert_eq!(file("update.rs")?["diff"]["firstLine"], 13);
    assert_eq!(file("update.rs")?["additions"], 1);
    assert_eq!(file("update.rs")?["deletions"], 1);
    assert_eq!(file("overwrite.txt")?["deletions"], 3);
    assert_eq!(file("overwrite.txt")?["additions"], 1);
    assert_eq!(file("delete.txt")?["deletions"], 2);
    assert_eq!(file("delete.txt")?["additions"], 0);
    assert_eq!(
        std::fs::read_to_string(root.0.join("update.rs")).map_err(|e| e.to_string())?,
        format!("{prefix}after\ntail\n")
    );
    assert!(!root.0.join("delete.txt").exists());
    // A changed source still refuses through the original effect owner.
    assert!(super::super::super::run(&root.0, &plan, &input).is_err());
    Ok(())
}
