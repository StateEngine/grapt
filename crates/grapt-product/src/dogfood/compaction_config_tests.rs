use super::*;

fn document(raw: &str) -> Result<ConfigValue, String> {
    let bounds = super::super::catalog_config::bounds()?;
    Ok(grapt_config::decode(raw.as_bytes(), bounds)
        .map_err(|error| error.to_string())?
        .into_value())
}

fn fold(raws: &[&str]) -> Result<CompactionSettings, String> {
    let documents = raws
        .iter()
        .map(|raw| document(raw))
        .collect::<Result<Vec<_>, _>>()?;
    CompactionSettings::fold(documents.iter())
}

#[test]
fn absent_config_keeps_se_defaults() -> Result<(), String> {
    let settings = fold(&[r#"{"model":"x"}"#])?;
    assert_eq!(settings, CompactionSettings::default());
    assert!(settings.auto);
    assert_eq!(settings.buffer, None);
    assert_eq!(
        settings.server_side, None,
        "server_side is unset by default"
    );
    Ok(())
}

/// The 2026-10-09 decision: unset, provider
/// compaction is on for the OpenAI families and off elsewhere; an explicit
/// value cannot override the provider policy.
#[test]
fn server_side_follows_provider_policy_even_with_legacy_overrides() -> Result<(), String> {
    use grapt_provider::ProtocolFamily::{
        AnthropicMessages, ChatGptCodex, OpenAiCompatible, OpenAiResponses,
    };
    let unset = fold(&[r#"{"model":"x"}"#])?;
    assert!(unset.server_side_on(OpenAiResponses));
    assert!(unset.server_side_on(ChatGptCodex));
    assert!(!unset.server_side_on(AnthropicMessages));
    assert!(
        !unset.server_side_on(OpenAiCompatible),
        "OpenRouter routes are openai-compatible"
    );
    let off = fold(&[r#"{"compaction":{"server_side":false}}"#])?;
    assert!(off.server_side_on(OpenAiResponses));
    assert!(off.server_side_on(ChatGptCodex));
    let on = fold(&[r#"{"compaction":{"server_side":true}}"#])?;
    assert!(
        !on.server_side_on(AnthropicMessages),
        "Anthropic always uses SE summary"
    );
    Ok(())
}

/// An explicit `server_side` folds like every key.
#[test]
fn server_side_folds_per_key() -> Result<(), String> {
    assert_eq!(
        fold(&[r#"{"compaction":{"server_side":true}}"#])?.server_side,
        Some(true)
    );
    let later = fold(&[
        r#"{"compaction":{"server_side":true}}"#,
        r#"{"compaction":{"server_side":false,"buffer":1}}"#,
    ])?;
    assert_eq!(later.server_side, Some(false));
    let kept = fold(&[
        r#"{"compaction":{"server_side":true}}"#,
        r#"{"compaction":{"buffer":1}}"#,
    ])?;
    assert_eq!(
        kept.server_side,
        Some(true),
        "a later document without the key reset it"
    );
    Ok(())
}

#[test]
fn a_later_document_wins_per_key_as_the_oracle_folds() -> Result<(), String> {
    let settings = fold(&[
        r#"{"compaction":{"auto":false,"buffer":1000}}"#,
        r#"{"compaction":{"buffer":209248}}"#,
    ])?;
    assert!(!settings.auto, "a later document without auto reset it");
    // The buffer moves no threshold: it is accepted and named as ignored.
    assert_eq!(settings.buffer, Some(209_248));
    assert_eq!(settings.ignored().collect::<Vec<_>>(), [BUFFER]);
    Ok(())
}

/// By ruling, `keep` is accepted so oracle configs load,
/// and has no effect; a Session that sets `keep.tokens` is told so.
#[test]
fn keep_tokens_are_accepted_and_ignored() -> Result<(), String> {
    let with_keep = fold(&[r#"{"compaction":{"keep":{"tokens":2000},"auto":true}}"#])?;
    let without = fold(&[r#"{"compaction":{"auto":true}}"#])?;
    assert_eq!(
        CompactionSettings {
            keep_tokens: false,
            ..with_keep
        },
        without
    );
    assert_eq!(with_keep.ignored().collect::<Vec<_>>(), [KEEP_TOKENS]);
    assert_eq!(without.ignored().count(), 0);
    // Any document that sets it is named; an empty `keep` sets nothing.
    let later = fold(&[
        r#"{"compaction":{"keep":{"tokens":2000}}}"#,
        r#"{"compaction":{"keep":{}}}"#,
    ])?;
    assert_eq!(later.ignored().collect::<Vec<_>>(), [KEEP_TOKENS]);
    assert_eq!(
        fold(&[r#"{"compaction":{"keep":{}}}"#])?.ignored().count(),
        0
    );
    Ok(())
}

#[test]
fn a_malformed_compaction_value_is_refused_by_name() -> Result<(), String> {
    for (raw, named) in [
        (r#"{"compaction":true}"#, "compaction must be an object"),
        (r#"{"compaction":{"auto":"no"}}"#, "compaction.auto"),
        (
            r#"{"compaction":{"server_side":"yes"}}"#,
            "compaction.server_side",
        ),
        (r#"{"compaction":{"buffer":-1}}"#, "compaction.buffer"),
        (r#"{"compaction":{"buffer":1.5}}"#, "compaction.buffer"),
        (
            r#"{"compaction":{"keep":{"tokens":"many"}}}"#,
            "compaction.keep.tokens",
        ),
    ] {
        let refused = fold(&[raw])
            .err()
            .ok_or_else(|| format!("{raw} was accepted"))?;
        assert!(refused.contains(named), "{raw}: {refused}");
    }
    Ok(())
}

#[test]
fn token_limit_folds_from_the_root_key_and_rejects_invalid_counts() -> Result<(), String> {
    let settings = fold(&[
        r#"{"model_auto_compact_token_limit":700000}"#,
        r#"{"model_auto_compact_token_limit":600000,"compaction":{"auto":false}}"#,
        r#"{"compaction":{"buffer":5000}}"#,
    ])?;
    assert_eq!(settings.model_auto_compact_token_limit, Some(600_000));
    for invalid in ["0", "-1", "1.5", "null", "\"600k\""] {
        let raw = format!("{{\"model_auto_compact_token_limit\":{invalid}}}");
        assert!(fold(&[&raw]).is_err());
    }
    Ok(())
}
