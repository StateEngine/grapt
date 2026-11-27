use super::*;

#[test]
fn tool01b_missing_reads_report_the_nearest_parent_after_list_authorization() -> Result<(), String>
{
    let scratch = Scratch::new()?;
    let root = &scratch.0;
    std::fs::create_dir(root.join("docs")).map_err(|error| error.to_string())?;
    std::fs::write(root.join("docs/existing.rs"), "source").map_err(|error| error.to_string())?;
    std::fs::write(root.join("root-only.txt"), "outside parent")
        .map_err(|error| error.to_string())?;
    for (tool, args) in [
        (
            READ_TOOL,
            serde_json::json!({"path":"docs/missing/file.rs"}),
        ),
        (
            READ_LINES_TOOL,
            serde_json::json!({"path":"docs/missing/file.rs"}),
        ),
        (
            GREP_TOOL,
            serde_json::json!({"path":"docs/missing","pattern":"needle"}),
        ),
        (
            GLOB_TOOL,
            serde_json::json!({"path":"docs/missing","pattern":"**"}),
        ),
        (LIST_TOOL, serde_json::json!({"path":"docs/missing"})),
    ] {
        let args = input(&args.to_string())?;
        let prepared = prepare_effect(root, tool, &args, &no_routes())?;
        assert_eq!(
            prepared.action, LIST_TOOL,
            "{tool} must authorize directory metadata"
        );
        assert_eq!(prepared.resource, path_text(&root.join("docs")));
        let EffectTarget::Coding(target) = prepared.target else {
            return Err("missing read used another effect path".into());
        };
        let error = coding::run(root, &target, &args)
            .err()
            .ok_or("missing read succeeded")?;
        assert!(
            error.contains("not found") && error.contains("nearest existing parent: docs"),
            "{tool}: {error}"
        );
        assert!(error.contains("docs/existing.rs"), "{tool}: {error}");
        assert!(
            !error.contains("use write") && !error.contains("root-only.txt"),
            "{tool}: {error}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn tool01b_a_replaced_parent_cannot_disclose_outside_entries() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let outside = Scratch::new()?;
    std::fs::create_dir(scratch.0.join("docs")).map_err(|error| error.to_string())?;
    std::fs::write(outside.0.join("outside-secret-name"), "private")
        .map_err(|error| error.to_string())?;
    let args = input(r#"{"path":"docs/missing"}"#)?;
    let prepared = prepare_effect(&scratch.0, READ_TOOL, &args, &no_routes())?;
    let EffectTarget::Coding(target) = prepared.target else {
        return Err("missing read did not prepare metadata".into());
    };
    std::fs::remove_dir(scratch.0.join("docs")).map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&outside.0, scratch.0.join("docs"))
        .map_err(|error| error.to_string())?;
    let error = coding::run(&scratch.0, &target, &args)
        .err()
        .ok_or("replaced parent was read")?;
    assert!(
        !error.contains("outside-secret-name"),
        "outside entry disclosed: {error}"
    );
    assert!(error.contains("changed after preparation"), "{error}");
    Ok(())
}

#[test]
fn tool01b_history_schema_distinguishes_listing_cursor_from_output_offset() -> Result<(), String> {
    let definition = tool_definition::<HistoryArguments>(HISTORY_DESCRIPTION);
    let schema: serde_json::Value =
        serde_json::from_str(&definition.input_schema).map_err(|error| error.to_string())?;
    let description = |field: &str| -> Result<&str, String> {
        schema["properties"][field]["description"]
            .as_str()
            .ok_or_else(|| format!("history {field} has no mode guidance"))
    };
    assert!(description("call")?.contains("Ignores cursor"));
    assert!(description("cursor")?.contains("Ignored when call is present"));
    assert!(description("offset")?.contains("Requires call"));
    assert!(HISTORY_DESCRIPTION.contains("call selects fetch and ignores cursor"));
    // Retain the flat provider schema and normalize in the preparation owner.
    assert!(schema.get("oneOf").is_none());
    let scratch = Scratch::new()?;
    let prepared = prepare_effect(
        &scratch.0,
        HISTORY_TOOL,
        &input(r#"{"call":"call_recorded","cursor":0,"offset":0}"#)?,
        &no_routes(),
    )?;
    let EffectTarget::History {
        call,
        cursor,
        offset,
    } = prepared.target
    else {
        return Err("history did not prepare a history target".into());
    };
    assert_eq!(call.as_ref().map(ToolCallId::as_str), Some("call_recorded"));
    assert_eq!(cursor, None);
    assert_eq!(offset, Some(0));
    Ok(())
}
