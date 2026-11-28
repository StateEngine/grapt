use super::super::{prepare_effect, EDIT_TOOL, READ_TOOL, SHELL_TOOL};
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn call(value: &serde_json::Value) -> Result<BoundedToolInput, Box<dyn std::error::Error>> {
    Ok(BoundedToolInput::new(serde_json::to_vec(value)?)?)
}

fn object(input: &BoundedToolInput) -> Result<Value, serde_json::Error> {
    serde_json::from_slice(input.as_bytes())
}

#[test]
fn aliases_rename_keys_and_never_touch_their_values() -> TestResult {
    let anchor = "  old\r\n\tline with file_path ";
    let (repaired, change) = repair(
        EDIT_TOOL,
        call(&serde_json::json!({
            "file_path": "src/../lib.rs", "old_string": anchor, "new_string": "new"
        }))?,
    );
    assert_eq!(change, Some(Repair::Alias));
    assert_eq!(
        object(&repaired)?,
        serde_json::json!({"path": "src/../lib.rs", "old_text": anchor, "new_text": "new"})
    );
    // With the declared name present the alias stays, and the hint names it.
    let both = call(&serde_json::json!({"path": "a", "file_path": "b"}))?;
    let (kept, change) = repair(READ_TOOL, both);
    assert_eq!(change, None);
    let (hint, detail) = parameter_hint(READ_TOOL, &kept).ok_or("no hint")?;
    assert_eq!(hint, Hint::UnknownParameter);
    assert!(
        detail.contains("`file_path` (did you mean `path`?)"),
        "{detail}"
    );
    Ok(())
}

#[test]
fn only_exact_textual_scalars_become_the_declared_type() -> TestResult {
    let (repaired, change) = repair(
        READ_TOOL,
        call(&serde_json::json!({"path": "7", "offset": "10", "limit": "5"}))?,
    );
    assert_eq!(change, Some(Repair::Coercion));
    assert_eq!(
        object(&repaired)?,
        serde_json::json!({"path": "7", "offset": 10, "limit": 5})
    );
    for kept in ["-1", "1e3", " 5", "0x5", "", "99999999999999999999"] {
        let (_, change) = repair(
            READ_TOOL,
            call(&serde_json::json!({"path": "a", "offset": kept}))?,
        );
        assert_eq!(change, None, "{kept:?}");
    }
    let (repaired, change) = repair(
        EDIT_TOOL,
        call(
            &serde_json::json!({"filePath": "a", "old_text": "x", "new_text": "y",
            "replace_all": "true"}),
        )?,
    );
    assert_eq!(change, Some(Repair::AliasAndCoercion));
    assert_eq!(object(&repaired)?["replace_all"], Value::Bool(true));
    let (_, change) = repair(
        EDIT_TOOL,
        call(
            &serde_json::json!({"path": "a", "old_text": "x", "new_text": "y",
            "replace_all": "True"}),
        )?,
    );
    assert_eq!(change, None);
    Ok(())
}

#[test]
fn shell_text_and_published_tools_are_never_rewritten() -> TestResult {
    let command = r#"echo "file_path" 10 true; rm -rf "$HOME/x""#;
    let (repaired, change) = repair(
        SHELL_TOOL,
        call(&serde_json::json!({"command": command, "timeout_ms": "1000"}))?,
    );
    assert_eq!(change, Some(Repair::Coercion));
    assert_eq!(object(&repaired)?["command"], Value::from(command));
    let foreign = call(&serde_json::json!({"file_path": "a", "limit": "5"}))?;
    let bytes = foreign.as_bytes().to_vec();
    let (kept, change) = repair("mcp_example", foreign);
    assert_eq!((change, kept.as_bytes()), (None, bytes.as_slice()));
    assert!(parameter_hint("mcp_example", &kept).is_none());
    Ok(())
}

/// Preparation, and so authorization, judges the repaired call exactly as it
/// judges the canonical one: a repaired path outside the project still refuses.
#[test]
fn preparation_sees_the_repaired_call() -> TestResult {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join("a.txt"), "text")?;
    let routes = std::collections::BTreeMap::new();
    let (aliased, _) = repair(READ_TOOL, call(&serde_json::json!({"file_path": "a.txt"}))?);
    let canonical = call(&serde_json::json!({"path": "a.txt"}))?;
    let repaired = prepare_effect(root.path(), READ_TOOL, &aliased, &routes)?;
    let expected = prepare_effect(root.path(), READ_TOOL, &canonical, &routes)?;
    assert_eq!(
        (repaired.action, repaired.resource),
        (expected.action, expected.resource)
    );
    let (outside, _) = repair(
        READ_TOOL,
        call(&serde_json::json!({"file_path": "../outside.txt"}))?,
    );
    assert!(prepare_effect(root.path(), READ_TOOL, &outside, &routes).is_err());
    Ok(())
}

#[test]
fn hints_name_missing_parameters_and_close_tools_without_values() -> TestResult {
    let (hint, detail) = parameter_hint(
        READ_TOOL,
        &call(&serde_json::json!({"secret": "token-value"}))?,
    )
    .ok_or("no hint")?;
    assert_eq!(hint, Hint::UnknownParameter);
    assert!(
        detail.contains("missing required parameter `path`"),
        "{detail}"
    );
    assert!(!detail.contains("token-value"), "{detail}");
    let (hint, _) = parameter_hint(READ_TOOL, &call(&serde_json::json!({}))?).ok_or("no hint")?;
    assert_eq!(hint, Hint::MissingParameter);
    assert!(parameter_hint(READ_TOOL, &call(&serde_json::json!({"path": "a"}))?).is_none());
    assert_eq!(unknown_tool_hint("raed"), " (did you mean `read`?)");
    assert_eq!(unknown_tool_hint("completely_unrelated"), "");
    Ok(())
}
