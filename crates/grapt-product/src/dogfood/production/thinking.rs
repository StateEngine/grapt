//! Coding-request thinking policy. Model discovery remains platform-owned.
//!
//! Adaptive thinking with summarized display.
//! Copyright 2023-2026 SpaceXAI. Apache-2.0.
//! Modified by Grapt: discovery-gated selection and a finite legacy budget.

use grapt_platform::{AnthropicThinkingMode, ConnectedModel};
use grapt_provider::{MaxOutputTokens, ThinkingConfig};

pub(super) fn configuration(row: Option<&ConnectedModel>) -> Option<ThinkingConfig> {
    Some(match row?.anthropic_thinking()? {
        AnthropicThinkingMode::Adaptive => ThinkingConfig::Adaptive,
        // Keep the established reasoning depth; bounded() fits it within the
        // selected model's total output ceiling before request construction.
        AnthropicThinkingMode::Manual => ThinkingConfig::Enabled {
            budget_tokens: 2048,
        },
    })
}

/// Read the canonical catalog value; a DTO's display fallback is not capacity.
pub(super) fn output_limit(row: Option<&ConnectedModel>) -> Result<MaxOutputTokens, String> {
    let metadata = row
        .ok_or("selected model catalog row is unavailable")?
        .metadata();
    let value = metadata
        .pointer("/limit/output")
        .ok_or("selected model output limit is unavailable")?;
    let tokens = value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("selected model output limit is invalid")?;
    MaxOutputTokens::new(tokens)
        .ok_or_else(|| "selected model output limit must be positive".into())
}

/// Missing capacity leaves the caller's explicit budget intact; malformed capacity refuses.
pub(super) fn output_bound(
    row: Option<&ConnectedModel>,
    requested: u32,
) -> Result<MaxOutputTokens, String> {
    let row = row.ok_or("selected model catalog row is unavailable")?;
    let bound = match row.metadata().pointer("/limit/output") {
        None | Some(serde_json::Value::Null) => requested,
        Some(_) => requested.min(output_limit(Some(row))?.value()),
    };
    MaxOutputTokens::new(bound).ok_or_else(|| "requested output budget must be positive".into())
}

/// Manual thinking shares max_tokens with the answer. Never emit an invalid
/// sub-minimum budget or silently disable a selected model's thinking mode.
pub(super) fn bounded(
    thinking: Option<ThinkingConfig>,
    max_tokens: MaxOutputTokens,
) -> Result<Option<ThinkingConfig>, String> {
    Ok(match thinking {
        Some(ThinkingConfig::Enabled { budget_tokens }) => {
            let budget_tokens = budget_tokens.min(max_tokens.value().saturating_sub(1));
            if budget_tokens < 1024 {
                return Err(
                    "selected model output limit cannot fit the minimum thinking budget".into(),
                );
            }
            Some(ThinkingConfig::Enabled { budget_tokens })
        }
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(output: Option<serde_json::Value>) -> Result<grapt_platform::ModelInventory, String> {
        let mut catalog = json!({"anthropic":{"id":"anthropic","models":{
            "claude-sonnet-4-5":{"id":"claude-sonnet-4-5","limit":{"context":200000}}
        }}});
        if let Some(output) = output {
            catalog["anthropic"]["models"]["claude-sonnet-4-5"]["limit"]["output"] = output;
        }
        grapt_platform::ModelInventory::first_run(
            catalog,
            json!({"data":[{"id":"claude-sonnet-4-5"}]}),
            "anthropic",
            "models.dev-anthropic",
            "anthropic-messages",
        )
    }

    #[test]
    fn catalog_output_is_positive_and_representable_without_fabricating_capacity(
    ) -> Result<(), String> {
        for tokens in [1025, 4096, 32000, 64000, 128000] {
            let inventory = model(Some(json!(tokens)))?;
            assert_eq!(output_limit(inventory.available().first())?.value(), tokens);
        }
        for invalid in [
            json!(null),
            json!(0),
            json!(-1),
            json!("64000"),
            json!(64000.5),
            json!(u64::MAX),
        ] {
            let inventory = model(Some(invalid))?;
            assert!(output_limit(inventory.available().first()).is_err());
        }
        let missing = model(None)?;
        assert!(output_limit(missing.available().first()).is_err());
        assert!(output_limit(None).is_err());
        Ok(())
    }

    #[test]
    fn thinking_and_answer_share_the_catalog_ceiling() -> Result<(), String> {
        for (total, expected) in [(1025, 1024), (2048, 2047), (4096, 2048), (64000, 2048)] {
            let limit = MaxOutputTokens::new(total).ok_or("output limit")?;
            assert_eq!(
                bounded(
                    Some(ThinkingConfig::Enabled {
                        budget_tokens: 2048
                    }),
                    limit
                )?,
                Some(ThinkingConfig::Enabled {
                    budget_tokens: expected
                })
            );
            assert_eq!(
                bounded(Some(ThinkingConfig::Adaptive), limit)?,
                Some(ThinkingConfig::Adaptive)
            );
            assert_eq!(bounded(None, limit)?, None);
        }
        for total in [1, 1024] {
            assert!(bounded(
                Some(ThinkingConfig::Enabled {
                    budget_tokens: 2048
                }),
                MaxOutputTokens::new(total).ok_or("output limit")?
            )
            .is_err());
        }
        Ok(())
    }
}
