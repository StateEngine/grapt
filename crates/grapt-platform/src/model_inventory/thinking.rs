//! Read-only Anthropic discovery interpretation; never grants model issuance.
//!
//! Anthropic Models API: capabilities.thinking.types.{adaptive,enabled}.supported.
//! Missing capability data uses only the exact documented model aliases below.
//! https://platform.claude.com/docs/en/build-with-claude/thinking (2026-10-15).

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnthropicThinkingMode {
    Adaptive,
    Manual,
}

pub(super) fn mode(id: &str, row: &Value) -> Result<Option<AnthropicThinkingMode>, String> {
    if row
        .get("capabilities")
        .is_some_and(|value| !value.is_null() && !value.is_object())
    {
        return Err("account model has invalid capabilities".into());
    }
    let thinking = row.pointer("/capabilities/thinking");
    if let Some(thinking) = thinking.filter(|value| !value.is_null()) {
        let supported = thinking
            .get("supported")
            .and_then(Value::as_bool)
            .ok_or("account model has invalid thinking capabilities")?;
        if !supported {
            return Ok(None);
        }
        let flag = |kind: &str| {
            thinking
                .get("types")
                .and_then(|types| types.get(kind))
                .and_then(|value| value.get("supported"))
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("account model has invalid {kind} thinking support"))
        };
        let adaptive = flag("adaptive")?;
        let enabled = flag("enabled")?;
        return Ok(if adaptive {
            Some(AnthropicThinkingMode::Adaptive)
        } else if enabled {
            Some(AnthropicThinkingMode::Manual)
        } else {
            None
        });
    }
    Ok(match id {
        "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-haiku-5-5" | "claude-fable-5-1"
        | "claude-mythos-5-1" | "claude-fable-5" | "claude-mythos-5" | "claude-opus-5"
        | "claude-sonnet-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6"
        | "claude-sonnet-4-6" => Some(AnthropicThinkingMode::Adaptive),
        "claude-opus-4-5" | "claude-sonnet-4-5" | "claude-haiku-4-5" => {
            Some(AnthropicThinkingMode::Manual)
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adaptive_and_legacy_modes_are_distinct_without_model_name_guessing() -> Result<(), String> {
        assert_eq!(
            mode("claude-opus-5-5", &Value::Null)?,
            Some(AnthropicThinkingMode::Adaptive)
        );
        assert_eq!(
            mode("claude-opus-4-6", &Value::Null)?,
            Some(AnthropicThinkingMode::Adaptive)
        );
        assert_eq!(
            mode("claude-sonnet-4-5", &Value::Null)?,
            Some(AnthropicThinkingMode::Manual)
        );
        for id in [
            "claude-live",
            "claude-opus-6",
            "claude-opus-5-5-custom",
            "other/claude-opus-5-5",
        ] {
            assert_eq!(mode(id, &Value::Null)?, None, "{id}");
        }
        Ok(())
    }

    #[test]
    fn discovery_overrides_fallback_and_supports_new_model_identities() -> Result<(), String> {
        let capabilities = |adaptive, enabled| {
            json!({"capabilities":{"thinking": {
                "supported":true,"types": {
                    "adaptive":{"supported":adaptive},"enabled":{"supported":enabled}
                }
            }}})
        };
        assert_eq!(
            mode("new-model", &capabilities(true, true))?,
            Some(AnthropicThinkingMode::Adaptive)
        );
        assert_eq!(
            mode("new-model", &capabilities(false, true))?,
            Some(AnthropicThinkingMode::Manual)
        );
        assert_eq!(mode("claude-opus-5-5", &capabilities(false, false))?, None);
        assert_eq!(
            mode(
                "claude-opus-5-5",
                &json!({"capabilities":{"thinking":{"supported":false}}})
            )?,
            None
        );
        Ok(())
    }

    #[test]
    fn malformed_support_is_not_silently_treated_as_disabled() {
        for thinking in [
            json!({}),
            json!({"supported":"true"}),
            json!({"supported":true,"types":{"adaptive":{"supported":true}}}),
        ] {
            assert!(mode(
                "claude-opus-5-5",
                &json!({"capabilities":{"thinking":thinking}})
            )
            .is_err());
        }
    }
    #[test]
    fn native_first_run_retains_capabilities_in_the_connected_model() -> Result<(), String> {
        let public = json!({"anthropic":{"id":"anthropic","models":{
            "new-native":{"id":"new-native","limit":{"context":200_000}}
        }}});
        let listing = json!({"data":[{"id":"new-native","capabilities":{"thinking":{
            "supported":true,"types":{
                "adaptive":{"supported":true},"enabled":{"supported":false}
            }
        }}}]});
        let inventory = super::super::ModelInventory::first_run(
            public.clone(),
            listing.clone(),
            "anthropic",
            "models.dev-anthropic",
            "anthropic-messages",
        )?;
        let row = inventory.available().first().ok_or("connected model")?;
        assert_eq!(
            row.anthropic_thinking(),
            Some(AnthropicThinkingMode::Adaptive)
        );
        assert_eq!(row.context(), Some(200_000));
        let mut malformed = listing;
        malformed["data"][0]["capabilities"]["thinking"]["types"]["adaptive"]["supported"] =
            json!("true");
        assert!(super::super::ModelInventory::first_run(
            public,
            malformed,
            "anthropic",
            "models.dev-anthropic",
            "anthropic-messages"
        )
        .is_err());
        Ok(())
    }
}
