//! Bounded presentation capabilities; canonical Product validation remains authoritative.
use super::*;
use grapt_client::wire::FormField;
use serde_json::Value;

pub(super) const MAX_OPTIONS: usize = 128;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Input {
    Text,
    Number,
    Flag,
    Choice,
    Multi,
}

pub(super) fn custom(field: &FormField) -> bool {
    field.attributes.get("custom") == Some(&Value::Bool(true))
}

pub(super) fn options(field: &FormField) -> &[Value] {
    field
        .attributes
        .get("options")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

pub(super) fn input(field: &FormField) -> Result<Input, String> {
    let kind = match field.kind.as_str() {
        "string" if field.attributes.contains_key("options") => Input::Choice,
        "string" => Input::Text,
        "number" | "integer" => Input::Number,
        "boolean" => Input::Flag,
        "multiselect" => Input::Multi,
        _ => return Err("Unsupported field type".into()),
    };
    for (key, value) in &field.attributes {
        let valid = match key.as_str() {
            "description" => value.is_string(),
            "when" => value
                .as_array()
                .is_some_and(|items| items.len() <= MAX_FIELDS),
            "pattern" if matches!(kind, Input::Text | Input::Choice) => value.is_string(),
            "default" => defaults::value(field).is_ok(),
            "format" if matches!(kind, Input::Text | Input::Choice) => value
                .as_str()
                .is_some_and(|format| matches!(format, "email" | "uri" | "date" | "date-time")),
            "minimum" | "maximum" if kind == Input::Number => {
                value.as_f64().is_some_and(f64::is_finite)
            }
            "minLength" | "maxLength" if matches!(kind, Input::Text | Input::Choice) => {
                value.as_u64().is_some()
            }
            "minItems" | "maxItems" if kind == Input::Multi => value.as_u64().is_some(),
            "custom" if matches!(kind, Input::Choice | Input::Multi) => value.is_boolean(),
            "options" if matches!(kind, Input::Choice | Input::Multi) => valid_options(value),
            _ => false,
        };
        if !valid {
            return Err(format!("Unsupported field semantics: {key}"));
        }
    }
    if matches!(kind, Input::Choice | Input::Multi) && options(field).is_empty() && !custom(field) {
        return Err("Unsupported choice field: no bounded options".into());
    }
    Ok(kind)
}

fn valid_options(value: &Value) -> bool {
    let Some(rows) = value.as_array().filter(|rows| rows.len() <= MAX_OPTIONS) else {
        return false;
    };
    let mut seen = std::collections::BTreeSet::new();
    rows.iter().all(|row| {
        let Some(object) = row.as_object() else {
            return false;
        };
        object
            .keys()
            .all(|key| matches!(key.as_str(), "value" | "label" | "description"))
            && object
                .iter()
                .all(|(_, value)| value.as_str().is_some_and(|text| text.len() <= MAX_FIELD))
            && row["value"].as_str().is_some_and(|text| seen.insert(text))
    })
}

pub(super) fn parse_number(text: &str) -> Result<serde_json::Number, String> {
    let text = text.trim();
    if text.len() > 128 {
        return Err("Number exceeds 128 bytes".into());
    }
    let number = serde_json::from_str::<serde_json::Number>(text)
        .map_err(|_| "Enter a finite JSON number".to_owned())?;
    // serde_json otherwise falls back to f64 for overflowing integer tokens.
    // Refuse that lossy conversion instead of rounding the user's integer.
    if text != "-0" && !text.contains(['.', 'e', 'E']) && !number.is_i64() && !number.is_u64() {
        return Err("Integer exceeds the supported exact range".into());
    }
    let coefficient = text.split(['e', 'E']).next().unwrap_or(text);
    if number.as_f64() == Some(0.0) && coefficient.chars().any(|ch| matches!(ch, '1'..='9')) {
        return Err("Number underflows the supported range".into());
    }
    Ok(number)
}

pub(super) fn validate_value(field: &FormField, value: &FormValue) -> Result<(), String> {
    let kind = input(field)?;
    let attributes = &field.attributes;
    let error = |note: &str| {
        Err(format!(
            "{note}: {}",
            grapt_tui_transcript::markdown::literal(&field.key)
        ))
    };
    match (kind, value) {
        (Input::Text | Input::Choice, FormValue::Text(text)) => {
            let length = text.encode_utf16().count() as u64;
            if text.len() > MAX_FIELD {
                return error("Field exceeds 16 KiB");
            }
            if field.required && text.is_empty() {
                return error("Still needed");
            }
            if attributes
                .get("minLength")
                .and_then(Value::as_u64)
                .is_some_and(|min| length < min)
            {
                return error("Text is too short");
            }
            if attributes
                .get("maxLength")
                .and_then(Value::as_u64)
                .is_some_and(|max| length > max)
            {
                return error("Text is too long");
            }
            if kind == Input::Choice
                && !custom(field)
                && !options(field)
                    .iter()
                    .any(|row| row["value"].as_str() == Some(text))
            {
                return error("Choose an offered option");
            }
        }
        (Input::Number, FormValue::Number(number)) => {
            let Some(number) = number.as_f64().filter(|number| number.is_finite()) else {
                return error("Enter a finite number");
            };
            if field.kind == "integer" && number.fract() != 0.0 {
                return error("Enter an integer");
            }
            if attributes
                .get("minimum")
                .and_then(Value::as_f64)
                .is_some_and(|min| number < min)
            {
                return error("Number is below minimum");
            }
            if attributes
                .get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|max| number > max)
            {
                return error("Number is above maximum");
            }
        }
        (Input::Flag, FormValue::Flag(_)) => {}
        (Input::Multi, FormValue::Texts(items)) => {
            let count = items.len() as u64;
            if items.len() > MAX_OPTIONS {
                return error("Too many selections");
            }
            if field.required && items.is_empty() {
                return error("Still needed");
            }
            if attributes
                .get("minItems")
                .and_then(Value::as_u64)
                .is_some_and(|min| count < min)
            {
                return error("Too few selections");
            }
            if attributes
                .get("maxItems")
                .and_then(Value::as_u64)
                .is_some_and(|max| count > max)
            {
                return error("Too many selections");
            }
            if !custom(field)
                && !items.iter().all(|item| {
                    options(field)
                        .iter()
                        .any(|row| row["value"].as_str() == Some(item))
                })
            {
                return error("Choose offered options");
            }
        }
        _ => return error("Answer does not match the field type"),
    }
    Ok(())
}

pub(super) fn hint(field: &FormField) -> String {
    if let Err(error) = input(field) {
        return error;
    }
    let mut notes = vec![match input(field) {
        Ok(Input::Number) => "Type/paste a JSON number".to_owned(),
        Ok(Input::Choice | Input::Multi) => {
            if custom(field) {
                "Ctrl+E custom text · ↑/↓ option · Space/Enter choose · Delete clear".to_owned()
            } else {
                "↑/↓ option · Space/Enter choose · Delete clear".to_owned()
            }
        }
        Ok(Input::Flag) => "Space toggles true/false · Delete clears".to_owned(),
        _ => "Type or paste text".to_owned(),
    }];
    for key in [
        "minimum",
        "maximum",
        "minLength",
        "maxLength",
        "minItems",
        "maxItems",
    ] {
        if let Some(value) = field.attributes.get(key) {
            notes.push(format!("{key}: {value}"));
        }
    }
    if let Some(format) = field.attributes.get("format").and_then(Value::as_str) {
        notes.push(format!("{format}; checked when you answer"));
    }
    if let Some(pattern) = field.attributes.get("pattern").and_then(Value::as_str) {
        notes.push(format!(
            "Pattern: {}; checked when you answer",
            grapt_client::cells::cut(&grapt_tui_transcript::markdown::literal(pattern), 120)
        ));
    }
    if let Some(value) = field.attributes.get("default") {
        notes.push(format!(
            "Default: {}",
            grapt_client::cells::cut(
                &grapt_tui_transcript::markdown::literal(&value.to_string()),
                120
            )
        ));
    }
    notes.join(" · ")
}
