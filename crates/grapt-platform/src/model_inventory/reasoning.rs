//! Offered reasoning controls, read from discovery before model selection.
//! Claude capability shape: https://platform.claude.com/docs/en/api/models/retrieve.
//! Exact fallback aliases follow the documented effort compatibility table.
use grapt_types::ModelVariantId;
use serde_json::Value;

pub(super) fn offered(
    provider: &str,
    id: &str,
    row: &Value,
) -> Result<Vec<ModelVariantId>, String> {
    let mut names = Vec::new();
    if let Some(levels) = row.get("supported_reasoning_levels") {
        let levels = levels
            .as_array()
            .ok_or("model reasoning levels are not an array")?;
        if levels.len() > super::MAX_VARIANTS {
            return Err("model reasoning levels exceed their bound".into());
        }
        for level in levels {
            names.push(
                level
                    .get("effort")
                    .and_then(Value::as_str)
                    .ok_or("reasoning level has no effort")?,
            );
        }
    } else if provider == "openai" {
        // Apache-2.0, Copyright OpenAI.
        // Only exact API-supported aliases; Ultra is a UI delegation mode,
        // not a wire effort. Unknown models keep their provider defaults.
        names.extend(match id {
            "gpt-6-astra"
            | "gpt-6.1-sol"
            | "gpt-6-sol"
            | "gpt-6-luna"
            | "gpt-5.6-sol"
            | "gpt-5.6-terra"
            | "gpt-5.6-luna"
            | "gpt-daybreak-blue-latest"
            | "gpt-daybreak-red-latest"
            | "codex-auto-review" => &["low", "medium", "high", "xhigh", "max"][..],
            "gpt-5.5" => &["low", "medium", "high", "xhigh"][..],
            _ => &[],
        });
    } else if provider == "anthropic" {
        if let Some(effort) = row
            .pointer("/capabilities/effort")
            .filter(|value| !value.is_null())
        {
            if effort
                .get("supported")
                .and_then(Value::as_bool)
                .ok_or("invalid effort support")?
            {
                for name in ["low", "medium", "high", "xhigh", "max"] {
                    if name == "xhigh" && effort.get(name).is_none_or(Value::is_null) {
                        continue;
                    }
                    if effort
                        .get(name)
                        .and_then(|level| level.get("supported"))
                        .and_then(Value::as_bool)
                        .ok_or("invalid effort level support")?
                    {
                        names.push(name);
                    }
                }
                if names.is_empty() {
                    return Err("effort support has no offered levels".into());
                }
            }
        } else {
            names.extend(match id {
                "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-haiku-5-5"
                | "claude-fable-5-1" | "claude-mythos-5-1" | "claude-fable-5"
                | "claude-mythos-5" | "claude-opus-5" | "claude-sonnet-5" | "claude-opus-4-8"
                | "claude-opus-4-7" => &["low", "medium", "high", "xhigh", "max"][..],
                "claude-opus-4-6" | "claude-sonnet-4-6" => &["low", "medium", "high", "max"][..],
                "claude-opus-4-5" => &["low", "medium", "high"][..],
                _ => &[],
            });
        }
    }
    let mut variants = Vec::new();
    for name in names {
        let variant = ModelVariantId::new(name).ok_or("reasoning effort identity is invalid")?;
        if variants.contains(&variant) {
            return Err("model reasoning level is repeated".into());
        }
        variants.push(variant);
    }
    Ok(variants)
}

pub(super) fn default_variant(
    row: &Value,
    variants: &[ModelVariantId],
) -> Result<Option<ModelVariantId>, String> {
    match row.get("default_reasoning_level") {
        None | Some(Value::Null) => Ok(None),
        // An account default outside its offered list stays unselected.
        // Explicit config/session choices still require catalog admission.
        Some(Value::String(value)) => Ok(variants
            .iter()
            .find(|effort| effort.as_str() == value)
            .cloned()),
        Some(_) => Err("default reasoning effort is not a string".into()),
    }
}

/// Explicit discovery defaults win; documented native defaults need an offered level.
pub(super) fn default_for(
    provider: &str,
    id: &str,
    row: &Value,
    variants: &[ModelVariantId],
) -> Result<Option<ModelVariantId>, String> {
    let explicit = default_variant(row, variants)?;
    if provider != "anthropic"
        || row
            .get("default_reasoning_level")
            .is_some_and(|value| !value.is_null())
    {
        return Ok(explicit);
    }
    let documented = match id {
        "claude-opus-5-5" | "claude-haiku-5-5" => "medium",
        "claude-fable-5-1" | "claude-mythos-5-1" | "claude-fable-5" | "claude-mythos-5"
        | "claude-opus-5" | "claude-sonnet-5-5" | "claude-sonnet-5" | "claude-opus-4-8"
        | "claude-opus-4-7" | "claude-opus-4-6" | "claude-sonnet-4-6" | "claude-opus-4-5" => "high",
        _ => return Ok(None),
    };
    Ok(variants
        .iter()
        .find(|variant| variant.as_str() == documented)
        .cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn documented_defaults_never_override_discovery_or_invent_unknown_values() -> Result<(), String>
    {
        for (id, expected) in [
            ("claude-opus-5-5", Some("medium")),
            ("claude-haiku-5-5", Some("medium")),
            ("claude-opus-4-6", Some("high")),
            ("unknown", None),
        ] {
            let variants = offered("anthropic", id, &Value::Null)?;
            assert_eq!(
                default_for("anthropic", id, &Value::Null, &variants)?
                    .as_ref()
                    .map(ModelVariantId::as_str),
                expected
            );
        }
        let variants = offered("anthropic", "claude-opus-5-5", &Value::Null)?;
        assert_eq!(
            default_for(
                "anthropic",
                "claude-opus-5-5",
                &json!({"default_reasoning_level":"low"}),
                &variants
            )?
            .as_ref()
            .map(ModelVariantId::as_str),
            Some("low")
        );
        assert_eq!(
            default_for(
                "anthropic",
                "claude-opus-5-5",
                &json!({"default_reasoning_level":"unsupported"}),
                &variants
            )?,
            None
        );
        assert_eq!(
            default_for("anthropic", "claude-opus-5-5", &Value::Null, &[])?,
            None
        );
        assert!(default_for(
            "anthropic",
            "claude-opus-5-5",
            &json!({"default_reasoning_level":true}),
            &variants
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn effort_capabilities_override_aliases_and_refuse_malformed_levels() -> Result<(), String> {
        assert_eq!(
            offered("anthropic", "claude-opus-5-5", &Value::Null)?.len(),
            5
        );
        assert!(offered("anthropic", "unknown", &Value::Null)?.is_empty());
        assert_eq!(offered("openai", "gpt-6.1-sol", &Value::Null)?.len(), 5);
        assert!(offered("openai", "gpt-6.1-sol-custom", &Value::Null)?.is_empty());
        assert_eq!(
            default_variant(&json!({"default_reasoning_level":"unsupported"}), &[])?,
            None
        );
        assert!(default_variant(&json!({"default_reasoning_level":true}), &[]).is_err());
        assert!(offered(
            "anthropic",
            "claude-opus-5-5",
            &json!({"capabilities":{"effort":{"supported":false}}})
        )?
        .is_empty());
        let mut row = json!({"capabilities":{"effort":{"supported":true,"low":{"supported":false},"medium":{"supported":false},"high":{"supported":true},"xhigh":{"supported":false},"max":{"supported":false}}}});
        assert_eq!(
            offered("anthropic", "unknown", &row)?
                .iter()
                .map(ModelVariantId::as_str)
                .collect::<Vec<_>>(),
            ["high"]
        );
        row["capabilities"]["effort"]["high"]["supported"] = json!("yes");
        assert!(offered("anthropic", "claude-opus-5-5", &row).is_err());
        Ok(())
    }
    #[test]
    fn effort_optional_xhigh_is_unoffered_but_malformed_support_refuses() -> Result<(), String> {
        let mut row = json!({"capabilities":{"effort":{"supported":true,
            "low":{"supported":true},"medium":{"supported":true},
            "high":{"supported":true},"max":{"supported":false}}}});
        for value in [None, Some(Value::Null)] {
            if let Some(value) = value {
                row["capabilities"]["effort"]["xhigh"] = value;
            }
            assert_eq!(
                offered("anthropic", "unknown", &row)?
                    .iter()
                    .map(ModelVariantId::as_str)
                    .collect::<Vec<_>>(),
                ["low", "medium", "high"]
            );
        }
        for value in [
            json!({}),
            json!({"supported":null}),
            json!({"supported":"yes"}),
            json!(false),
        ] {
            row["capabilities"]["effort"]["xhigh"] = value;
            assert!(offered("anthropic", "unknown", &row).is_err());
        }
        Ok(())
    }
    #[test]
    fn effort_is_retained_from_the_real_claude_discovery_shape() -> Result<(), String> {
        let public = json!({"anthropic":{"id":"anthropic","models":{"new-model":{"id":"new-model","limit":{"context":200000}}}}});
        let row = json!({"data":[{"id":"new-model","capabilities":{"effort":{"supported":true,"low":{"supported":false},"medium":{"supported":false},"high":{"supported":true},"xhigh":{"supported":false},"max":{"supported":false}}}}]});
        let inventory = super::super::ModelInventory::first_run(
            public,
            row,
            "anthropic",
            "models.dev-anthropic",
            "anthropic-messages",
        )?;
        let model = inventory.available().first().ok_or("model")?;
        assert_eq!(
            model
                .variants()
                .iter()
                .map(ModelVariantId::as_str)
                .collect::<Vec<_>>(),
            ["high"]
        );
        Ok(())
    }
    #[test]
    fn effort_listing_is_bounded_and_duplicate_values_refuse() {
        for levels in [
            json!(null),
            json!([{"effort":"high"},{"effort":"high"}]),
            json!([{}]),
            json!(vec![json!({"effort":"high"}); 33]),
        ] {
            assert!(offered(
                "openai",
                "any",
                &json!({"supported_reasoning_levels":levels})
            )
            .is_err());
        }
    }
}
