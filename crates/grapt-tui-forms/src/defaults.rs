//! Default selection for elicitation forms.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Modified by Grapt: seed bounded typed drafts once; never submit or reseed on polling.
use super::*;
use grapt_client::wire::FormField;
use serde_json::Value;

pub(super) fn value(field: &FormField) -> Result<Option<FormValue>, String> {
    let Some(value) = field.attributes.get("default") else {
        return Ok(None);
    };
    let text = |value: &Value| {
        value
            .as_str()
            .filter(|text| {
                text.len() <= MAX_FIELD
                    && !text
                        .chars()
                        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
            })
            .map(str::to_owned)
    };
    let value = match field.kind.as_str() {
        "string" => text(value).map(FormValue::Text),
        "number" | "integer" => value
            .as_number()
            .filter(|number| fields::parse_number(&number.to_string()).is_ok())
            .cloned()
            .map(FormValue::Number),
        "boolean" => value.as_bool().map(FormValue::Flag),
        "multiselect" => value
            .as_array()
            .filter(|items| {
                items.len() <= fields::MAX_OPTIONS && value.to_string().len() <= MAX_ANSWER
            })
            .and_then(|items| items.iter().map(text).collect::<Option<Vec<_>>>())
            .map(FormValue::Texts),
        _ => None,
    };
    value
        .map(Some)
        .ok_or_else(|| "Unsupported or oversized default".into())
}

pub(super) fn only_defaults_changed(old: &FormRow, new: &FormRow) -> bool {
    old.id == new.id
        && old.owner == new.owner
        && old.title == new.title
        && old.metadata == new.metadata
        && old.fields.len() == new.fields.len()
        && old.fields.iter().zip(&new.fields).all(|(old, new)| {
            old.key == new.key
                && old.kind == new.kind
                && old.title == new.title
                && old.required == new.required
                && old
                    .attributes
                    .iter()
                    .filter(|(key, _)| key.as_str() != "default")
                    .eq(new
                        .attributes
                        .iter()
                        .filter(|(key, _)| key.as_str() != "default"))
        })
}

impl Draft {
    pub(super) fn seed_defaults(&mut self) {
        for field in self.row.fields.iter().take(MAX_FIELDS) {
            if fields::input(field).is_err() {
                continue;
            }
            if let Ok(Some(value)) = value(field) {
                self.values.insert(field.key.clone(), value);
            }
        }
        let bytes = self
            .values
            .iter()
            .map(|(key, value)| key.len() + value.to_json().to_string().len() + 4)
            .sum::<usize>()
            + 13;
        if bytes > MAX_ANSWER {
            self.values.clear();
            self.note = "Defaults exceed the 64 KiB answer bound; enter answers explicitly".into();
            return;
        }
        if !self.values.is_empty() {
            self.note =
                "Defaults filled; review or edit, then Ctrl+S answers. Ctrl+R declines".into();
        }
        for field in &self.row.fields {
            match self.values.get(&field.key) {
                Some(FormValue::Text(text)) if field.attributes.contains_key("options") => {
                    if let Some(index) = fields::options(field)
                        .iter()
                        .position(|option| option["value"].as_str() == Some(text))
                    {
                        self.choices.insert(field.key.clone(), index);
                    }
                }
                Some(FormValue::Text(text)) => {
                    self.inputs.insert(field.key.clone(), text.clone());
                }
                Some(FormValue::Number(number)) => {
                    self.inputs.insert(field.key.clone(), number.to_string());
                }
                Some(FormValue::Texts(items)) => {
                    if let Some(index) = items.first().and_then(|value| {
                        fields::options(field)
                            .iter()
                            .position(|option| option["value"].as_str() == Some(value))
                    }) {
                        self.choices.insert(field.key.clone(), index);
                    }
                }
                _ => {}
            }
        }
        self.restore();
    }
}
