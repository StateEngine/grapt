//! SC-SEC-01: profile contents are not a model-readable or editable surface.
use super::*;

#[test]
fn profile_files_refuse_reads_and_writes_through_public_tool_inputs() -> Result<(), String> {
    let scratch = Scratch::new("profile-protection")?;
    let root = &scratch.0;
    for directory in [".grapt", "nested/.grapt"] {
        std::fs::create_dir_all(root.join(directory)).map_err(|error| error.to_string())?;
        let path = format!("{directory}/provider.credential");
        std::fs::write(root.join(&path), b"fixture-secret").map_err(|error| error.to_string())?;
        for (tool, value) in [
            (READ_TOOL, serde_json::json!({"path":path})),
            (READ_LINES_TOOL, serde_json::json!({"path":path})),
            (
                GREP_TOOL,
                serde_json::json!({"path":path,"pattern":"fixture","include_ignored":true}),
            ),
            (
                WRITE_TOOL,
                serde_json::json!({"path":path,"content":"changed"}),
            ),
            (
                EDIT_TOOL,
                serde_json::json!({"path":path,"old_text":"fixture","new_text":"changed"}),
            ),
        ] {
            let value = input(value)?;
            let error = prepare_effect(root, tool, &value, &std::collections::BTreeMap::new())
                .err()
                .ok_or_else(|| format!("{tool} exposed {path}"))?;
            assert!(
                error.contains("protected control metadata"),
                "{tool}: {error}"
            );
        }
        for tool in [GLOB_TOOL, LIST_TOOL] {
            let value = if tool == GLOB_TOOL {
                serde_json::json!({"path":directory,"pattern":"**","include_ignored":true})
            } else {
                serde_json::json!({"path":directory,"include_ignored":true})
            };
            assert!(
                call(root, tool, value).is_err(),
                "{tool} exposed {directory}"
            );
        }
        assert_eq!(
            std::fs::read(root.join(path)).map_err(|error| error.to_string())?,
            b"fixture-secret"
        );
    }
    Ok(())
}

#[test]
fn including_ignored_files_never_includes_profile_contents() -> Result<(), String> {
    let scratch = Scratch::new("profile-discovery")?;
    let root = &scratch.0;
    for directory in [".grapt", "nested/.grapt"] {
        std::fs::create_dir_all(root.join(directory)).map_err(|error| error.to_string())?;
        std::fs::write(
            root.join(directory).join("private-fixture"),
            b"fixture-secret",
        )
        .map_err(|error| error.to_string())?;
    }
    std::fs::write(root.join("ordinary.txt"), b"fixture-visible")
        .map_err(|error| error.to_string())?;
    for (tool, value) in [
        (
            GLOB_TOOL,
            serde_json::json!({"pattern":"**","include_ignored":true}),
        ),
        (
            GREP_TOOL,
            serde_json::json!({"pattern":"fixture","include_ignored":true}),
        ),
        (LIST_TOOL, serde_json::json!({"include_ignored":true})),
    ] {
        let output = call(root, tool, value)?.to_string();
        assert!(!output.contains("private-fixture"), "{tool}: {output}");
        assert!(!output.contains("fixture-secret"), "{tool}: {output}");
        assert!(output.contains("ordinary.txt"), "{tool}: {output}");
    }
    Ok(())
}
