//! The list path accepts a null directory under forced-required strict mode.
use super::*;

#[test]
fn strict_mode_null_fill_lists_the_working_directory() -> Result<(), String> {
    let mut schema: serde_json::Value = serde_json::from_str(&ListArguments::provider_schema())
        .map_err(|error| error.to_string())?;
    for keyword in ["anyOf", "oneOf", "allOf"] {
        assert!(schema.get(keyword).is_none());
    }
    schema["required"] = serde_json::json!(["path", "limit", "include_ignored"]);
    let value = serde_json::json!({"path":null,"limit":50,"include_ignored":false});
    schema::validate(
        &schema.to_string(),
        &serde_json::to_vec(&value).map_err(|error| error.to_string())?,
    )?;
    let scratch = Scratch::new("list_args")?;
    let input =
        BoundedToolInput::new(value.to_string().into_bytes()).map_err(|error| error.to_string())?;
    let prepared = prepare(&scratch.0, LIST_TOOL, &input)?;
    assert!(
        matches!(prepared.target, EffectTarget::Coding(CodingTarget::List(_))),
        "{value}"
    );
    Ok(())
}
