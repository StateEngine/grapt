//! Read-only status metadata through the existing public DTOs.
use super::*;

pub(super) fn context_window(
    model: &grapt_api::u3a::ModelInfoDto,
) -> Result<Option<u64>, WireError> {
    let value = serde_json::to_value(model.limit().get().context().get()).map_err(|error| {
        WireError::Codec {
            reason: error.to_string(),
        }
    })?;
    let count = value.as_u64().ok_or_else(|| WireError::Invalid {
        what: "model context capacity",
        value: value.to_string(),
    })?;
    Ok((count > 0).then_some(count))
}

/// Reported capabilities, with fields the catalog names as unknown kept
/// unknown rather than read as the retained DTO's false/empty spelling.
pub(super) fn capabilities(
    model: &grapt_api::u3a::ModelInfoDto,
) -> Result<ModelCapabilities, WireError> {
    let value =
        serde_json::to_value(model.capabilities().get()).map_err(|error| WireError::Codec {
            reason: error.to_string(),
        })?;
    let tools = value
        .get("tools")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| WireError::Invalid {
            what: "model tools capability",
            value: value.to_string(),
        })?;
    let text = |side: &str| -> Result<bool, WireError> {
        let items = value
            .get(side)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| WireError::Invalid {
                what: "model modality list",
                value: value.to_string(),
            })?;
        Ok(items.iter().any(|item| item.as_str() == Some("text")))
    };
    let mut reported = ModelCapabilities {
        tools: Some(tools),
        input_text: Some(text("input")?),
        output_text: Some(text("output")?),
    };
    for name in unknown_fields(model)? {
        match name.as_str() {
            "tools" => reported.tools = None,
            "input" => reported.input_text = None,
            "output" => reported.output_text = None,
            _ => {}
        }
    }
    Ok(reported)
}

/// The catalog's own list of fields it did not report, when it names one.
fn unknown_fields(model: &grapt_api::u3a::ModelInfoDto) -> Result<Vec<String>, WireError> {
    let Some(settings) = model.settings().as_present() else {
        return Ok(Vec::new());
    };
    let settings = serde_json::to_value(settings).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    let Some(unknown) = settings
        .get("metadata")
        .and_then(|metadata| metadata.get("unknown"))
    else {
        return Ok(Vec::new());
    };
    if unknown.is_null() {
        return Ok(Vec::new());
    }
    unknown
        .as_array()
        .ok_or_else(|| WireError::Invalid {
            what: "unknown metadata fields",
            value: unknown.to_string(),
        })?
        .iter()
        .map(|name| {
            name.as_str()
                .filter(|name| !name.is_empty() && name.len() <= 64)
                .map(str::to_owned)
                .ok_or_else(|| WireError::Invalid {
                    what: "unknown metadata field name",
                    value: name.to_string(),
                })
        })
        .collect()
}

/// Read only the selected provider's non-secret configured account label.
pub fn decode_provider_account(
    bytes: &[u8],
    provider: &ProviderWireId,
) -> Result<Option<String>, WireError> {
    let dto: grapt_api::u3a::ProviderListResponseDto = decode_page(bytes, limits()?)?;
    let mut matches = dto
        .data()
        .get()
        .as_slice()
        .iter()
        .filter(|row| row.id().get() == provider);
    let Some(row) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(WireError::Invalid {
            what: "unique provider account",
            value: provider.spelled().to_owned(),
        });
    }
    let Some(settings) = row.settings().as_present() else {
        return Ok(None);
    };
    let settings = serde_json::to_value(settings).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    match settings.get("account") {
        None => Ok(None),
        Some(serde_json::Value::String(label))
            if label.len() <= 256
                && !label.chars().any(char::is_control)
                && grapt_types::AccountId::new(label).is_some() =>
        {
            Ok(Some(label.clone()))
        }
        Some(_) => Err(WireError::Invalid {
            what: "provider account label",
            value: "unreadable".to_owned(),
        }),
    }
}

/// Catalog fact: the model preset's default reasoning effort.
/// Apache-2.0, Copyright 2025 OpenAI; licenses/openai-codex/).
pub(super) fn default_effort(
    model: &grapt_api::u3a::ModelInfoDto,
) -> Result<Option<ModelVariantWireId>, WireError> {
    let Some(settings) = model.settings().as_present() else {
        return Ok(None);
    };
    let settings = serde_json::to_value(settings).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    let Some(value) = settings
        .get("metadata")
        .and_then(|metadata| metadata.get("default_effort"))
    else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let effort = value
        .as_str()
        .filter(|value| !value.chars().any(char::is_control))
        .and_then(|value| ModelVariantWireId::parse(value.to_owned()));
    effort.map(Some).ok_or_else(|| WireError::Invalid {
        what: "default model effort",
        value: "unreadable".into(),
    })
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    use serde_json::{json, Value};

    fn model(unknown: Value) -> Value {
        json!({"id":"model","modelID":"model","providerID":"custom","name":"Model",
            "capabilities":{"tools":false,"input":[],"output":[]},
            "variants":[],"time":{"released":0.0},"cost":[],"status":"active","enabled":true,
            "limit":{"context":0,"output":0},
            "settings":{"metadata":{"source":"provider-listing","unknown":unknown}}})
    }

    #[test]
    fn named_unknowns_stay_unknown_and_reported_fields_are_facts(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dto: grapt_api::u3a::ModelInfoDto =
            serde_json::from_value(model(json!(["tools", "input", "output"])))?;
        let reported = capabilities(&dto)?;
        assert_eq!(reported.tools, None);
        assert_eq!(reported.input_text, None);
        assert_eq!(reported.output_text, None);

        let dto: grapt_api::u3a::ModelInfoDto = serde_json::from_value(model(json!([])))?;
        let reported = capabilities(&dto)?;
        assert_eq!(reported.tools, Some(false));
        assert_eq!(reported.input_text, Some(false));

        // Without an unknown list the retained DTO spelling is the report.
        let mut value = model(json!([]));
        value["settings"] = json!({});
        value["capabilities"] = json!({"tools":true,"input":["text"],"output":["text"]});
        let dto: grapt_api::u3a::ModelInfoDto = serde_json::from_value(value)?;
        let reported = capabilities(&dto)?;
        assert_eq!(reported.tools, Some(true));
        assert_eq!(reported.input_text, Some(true));
        assert_eq!(reported.output_text, Some(true));
        Ok(())
    }
}

#[cfg(test)]
mod effort_tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn default_effort_is_a_validated_catalog_fact_not_a_selection(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for (value, expected) in [(Value::Null, None), (json!("high"), Some("high"))] {
            let model: grapt_api::u3a::ModelInfoDto = serde_json::from_value(model(value))?;
            assert_eq!(
                default_effort(&model)?.as_ref().map(Spelled::spelled),
                expected
            );
        }
        for value in [json!(""), json!("high\n"), json!(42), json!({})] {
            let model: grapt_api::u3a::ModelInfoDto = serde_json::from_value(model(value))?;
            assert!(default_effort(&model).is_err());
        }
        Ok(())
    }

    fn model(effort: Value) -> Value {
        json!({"id":"model","modelID":"model","providerID":"provider","name":"Model",
            "capabilities":{"tools":true,"input":["text"],"output":["text"]},
            "variants":[{"id":"low"},{"id":"high"}],"time":{"released":0.0},
            "cost":[],"status":"active","enabled":true,"limit":{"context":200000,"output":16000},
            "settings":{"metadata":{"default_effort":effort}}})
    }
}
