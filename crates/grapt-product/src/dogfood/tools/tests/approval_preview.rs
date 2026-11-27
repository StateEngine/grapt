use super::*;
use crate::dogfood::tools::tests::Scratch;

fn prepared(
    root: &Path,
    tool: &str,
    value: Value,
) -> Result<(PreparedEffect, BoundedToolInput), String> {
    let input =
        BoundedToolInput::new(serde_json::to_vec(&value).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let effect = prepare_effect(root, tool, &input, &std::collections::BTreeMap::new())?;
    Ok((effect, input))
}

#[test]
fn shell_preview_uses_the_prepared_command_program_and_workdir_without_running(
) -> Result<(), String> {
    let scratch = Scratch::new()?;
    std::fs::create_dir(scratch.0.join("nested")).map_err(|error| error.to_string())?;
    for (tool, value, expected) in [
        (
            SHELL_TOOL,
            json!({"command":"echo approval-preview"}),
            scratch.0.clone(),
        ),
        (
            terminal::EXEC_COMMAND,
            json!({"cmd":"echo approval-preview", "workdir":"nested"}),
            scratch.0.join("nested"),
        ),
    ] {
        let (effect, input) = prepared(&scratch.0, tool, value)?;
        let preview = build(&scratch.0, &effect, &input)?.ok_or("preview")?;
        assert_eq!(preview["type"], "shell");
        assert_eq!(preview["command"], "echo approval-preview");
        assert_eq!(preview["cwd"], expected.to_str().ok_or("cwd")?);
        assert!(!preview["shell"].as_str().ok_or("shell")?.is_empty());
        assert_eq!(preview["truncated"], false);
        assert!(preview.get("classifierVerdict").is_none());
    }
    Ok(())
}

#[test]
fn patch_and_replacement_previews_are_the_proposed_input_without_file_writes() -> Result<(), String>
{
    let scratch = Scratch::new()?;
    let patch = "*** Begin Patch\n*** Add File: new.txt\n+proposed\n*** End Patch";
    let (effect, input) = prepared(&scratch.0, APPLY_PATCH_TOOL, json!({"input":patch}))?;
    let preview = build(&scratch.0, &effect, &input)?.ok_or("patch preview")?;
    assert_eq!(preview["format"], "apply-patch");
    assert_eq!(preview["patch"], patch);
    assert!(!scratch.0.join("new.txt").exists());
    std::fs::write(scratch.0.join("edit.txt"), "before\n").map_err(|error| error.to_string())?;
    let (effect, input) = prepared(
        &scratch.0,
        EDIT_TOOL,
        json!({"path":"edit.txt","old_text":"before","new_text":"after"}),
    )?;
    let preview = build(&scratch.0, &effect, &input)?.ok_or("edit preview")?;
    let text = preview["patch"].as_str().ok_or("patch text")?;
    assert!(text.contains("-before\n") && text.contains("+after\n"));
    assert_eq!(preview["format"], "replacement");
    assert_eq!(
        std::fs::read_to_string(scratch.0.join("edit.txt")).map_err(|e| e.to_string())?,
        "before\n"
    );
    for (tool, value, expected) in [
        (
            WRITE_TOOL,
            json!({"path":"write.txt", "content":"proposed\n"}),
            "write",
        ),
        (
            PATCH_TOOL,
            json!({"operations":[{"op":"create", "path":"ops.txt", "content":"proposed"}]}),
            "operations",
        ),
    ] {
        let (effect, input) = prepared(&scratch.0, tool, value)?;
        let preview = build(&scratch.0, &effect, &input)?.ok_or("file preview")?;
        assert_eq!(preview["format"], expected);
        assert!(preview["patch"]
            .as_str()
            .ok_or("patch")?
            .contains("proposed"));
    }
    assert!(!scratch.0.join("write.txt").exists());
    assert!(!scratch.0.join("ops.txt").exists());
    Ok(())
}

#[test]
fn preview_clipping_is_utf8_safe_and_respects_the_encoded_metadata_bound() -> Result<(), String> {
    let scratch = Scratch::new()?;
    for suffix in ["界".repeat(12_000), "\\".repeat(24_000)] {
        let command = format!("echo {suffix}");
        let (effect, input) = prepared(&scratch.0, SHELL_TOOL, json!({"command":command}))?;
        let request = metadata(&scratch.0, &effect, &input, None)
            .ok_or("oversized command preview must degrade, not fail")?;
        let preview = &request["approvalPreview"];
        assert_eq!(preview["truncated"], true);
        let shown = preview["command"].as_str().ok_or("command")?;
        assert!(shown.ends_with(" more bytes"), "{shown}");
        let (head, marker) = shown.rsplit_once('\n').ok_or("marker line")?;
        assert!(marker.starts_with('\u{2026}'), "{marker}");
        assert!(
            command.starts_with(head),
            "visible preview is not a prefix of the command"
        );
        let EffectTarget::Shell { command: bound, .. } = &effect.target else {
            panic!("shell");
        };
        assert_eq!(
            bound, &command,
            "display clipping cannot change the authorized command"
        );
    }
    Ok(())
}

#[test]
fn native_patch_metadata_overflow_truncates_and_the_full_patch_still_applies() -> Result<(), String>
{
    let scratch = Scratch::new()?;
    let root = scratch.0.join("preview-envelope-".repeat(8));
    std::fs::create_dir(&root).map_err(|error| error.to_string())?;
    let patch = |size| {
        format!(
            "*** Begin Patch\n*** Add File: new.txt\n+{}\n*** End Patch\n",
            "x".repeat(size)
        )
    };
    let source = patch(8_000);
    let (effect, input) = prepared(&root, APPLY_PATCH_TOOL, json!({"input":source.clone()}))?;
    let EffectTarget::Coding(coding::CodingTarget::Patch(plan)) = &effect.target else {
        return Err("native patch did not prepare a patch plan".into());
    };
    let consequences = plan.consequences();
    let request = metadata(&root, &effect, &input, Some(&consequences))
        .ok_or("oversized native patch preview must degrade, not fail")?;
    let preview = &request["approvalPreview"];
    assert_eq!(preview["type"], "patch");
    assert_eq!(preview["truncated"], true);
    let shown = preview["patch"].as_str().ok_or("patch text")?;
    let (head, marker) = shown.rsplit_once('\n').ok_or("marker line")?;
    assert!(marker.starts_with('\u{2026}'), "{marker}");
    assert!(marker.ends_with(" more bytes"), "{marker}");
    assert!(
        source.starts_with(head),
        "visible preview is not a prefix of the proposed patch"
    );
    let omitted: usize = marker
        .trim_start_matches('\u{2026}')
        .trim_start()
        .split(' ')
        .next()
        .ok_or("marker bytes")?
        .parse()
        .map_err(|_| "marker bytes")?;
    assert_eq!(
        source.len() - head.len(),
        omitted,
        "the marker must name the omitted bytes exactly"
    );
    assert!(!root.join("new.txt").exists());
    // The preview clipped bytes for display, not for execution: the full
    // proposed patch still applies.
    let EffectTarget::Coding(target) = &effect.target else {
        return Err("patch lost its coding target".into());
    };
    coding::run(&root, target, &input)?;
    assert_eq!(
        std::fs::read_to_string(root.join("new.txt")).map_err(|error| error.to_string())?,
        format!("{}\n", "x".repeat(8_000))
    );

    let (effect, input) = prepared(&root, APPLY_PATCH_TOOL, json!({"input":patch(256)}))?;
    let EffectTarget::Coding(coding::CodingTarget::Patch(plan)) = &effect.target else {
        return Err("smaller native patch did not prepare a patch plan".into());
    };
    let request = metadata(&root, &effect, &input, Some(&plan.consequences()))
        .ok_or("smaller native patch lost its approval metadata")?;
    assert_eq!(request["approvalPreview"]["truncated"], false);
    assert_eq!(request["approvalPreview"]["patch"], patch(256));
    Ok(())
}

#[test]
fn a_preview_that_cannot_fit_the_policy_envelope_is_dropped_without_failing() -> Result<(), String>
{
    let scratch = Scratch::new()?;
    let (effect, input) = prepared(&scratch.0, SHELL_TOOL, json!({"command":"pwd"}))?;
    let consequences = vec![grapt_permission::PermissionConsequence {
        action: "fs.modify".into(),
        resource: "x".repeat(grapt_api::u3a::limits::MAX_METADATA_BYTES),
    }];
    assert!(
        metadata(&scratch.0, &effect, &input, Some(&consequences)).is_none(),
        "an unfittable preview must be dropped, not fail the call"
    );
    Ok(())
}
