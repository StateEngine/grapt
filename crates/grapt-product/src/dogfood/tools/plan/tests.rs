use super::*;
fn input(raw: &str) -> Result<BoundedToolInput, String> {
    BoundedToolInput::new(raw.as_bytes().to_vec()).map_err(|error| error.to_string())
}

#[test]
fn checklist_is_a_builtin_with_codex_arguments_and_no_external_effect() -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let raw = r#"{"explanation":"Start with the failing case","plan":[{"step":"Reproduce","status":"completed"},{"step":"Fix","status":"in_progress"},{"step":"Verify","status":"pending"}]}"#;
    let prepared = prepare_effect(
        root,
        PLAN_TOOL,
        &input(raw)?,
        &std::collections::BTreeMap::new(),
    )?;
    assert_eq!(prepared.action, PLAN_TOOL);
    assert!(BUILTIN_TOOL_NAMES.contains(&PLAN_TOOL));
    let EffectTarget::Plan = prepared.target else {
        return Err("checklist did not use the governed executor".into());
    };
    let decoded: UpdatePlanArgs = decode_input(&input(raw)?)?;
    let output = serde_json::to_value(decoded).map_err(|e| e.to_string())?;
    let expected: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    assert_eq!(output, expected);
    Ok(())
}

#[test]
fn empty_checklist_explicitly_clears_the_plan() -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for raw in [r#"{"plan":[]}"#, r#"{"explanation":null,"plan":[]}"#] {
        let prepared = prepare(root, &input(raw)?)?;
        assert!(matches!(prepared.target, EffectTarget::Plan));
    }
    Ok(())
}

#[test]
fn invalid_or_ambiguous_checklists_fail_before_execution() -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for raw in [
        r#"{}"#,
        r#"{"plan":[{"step":"work","status":"done"}]}"#,
        r#"{"plan":[{"step":" ","status":"pending"}]}"#,
        r#"{"plan":[{"step":"a","status":"in_progress"},{"step":"b","status":"in_progress"}]}"#,
        r#"{"plan":[],"execute":true}"#,
        r#"{"plan":[{"step":"a","status":"pending","command":"do it"}]}"#,
    ] {
        assert!(prepare(root, &input(raw)?).is_err(), "accepted {raw}");
    }
    Ok(())
}

#[test]
fn checklist_counts_and_text_are_bounded() -> Result<(), String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for value in [
        serde_json::json!({"plan":vec![serde_json::json!({"step":"task","status":"pending"});65]}),
        serde_json::json!({"plan":[{"step":"é".repeat(513),"status":"pending"}]}),
        serde_json::json!({"plan":[],"explanation":"x".repeat(4097)}),
    ] {
        assert!(prepare(root, &input(&value.to_string())?).is_err());
    }
    let value = serde_json::json!({"plan":vec![serde_json::json!({"step":"x".repeat(1024),"status":"pending"});64],"explanation":"x".repeat(4096)});
    assert!(prepare(root, &input(&value.to_string())?).is_ok());
    Ok(())
}
