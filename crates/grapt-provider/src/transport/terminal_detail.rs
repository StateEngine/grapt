//! Bounded diagnosis excluding prompt and response content. Usage keeps its wire shape.
use super::error_text::{mask, ProviderErrorDetail};
use crate::credential::CredentialLease;
use serde_json::{Map, Value};

pub const TERMINAL_DETAIL_BYTES: usize = 4096;

pub struct MaskedTerminalFrame(String);
impl std::fmt::Debug for MaskedTerminalFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MaskedTerminalFrame([local diagnostic])")
    }
}
impl MaskedTerminalFrame {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) struct ErrorRedactor(Option<CredentialLease>);
impl std::fmt::Debug for ErrorRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ErrorRedactor([redacted])")
    }
}
impl ErrorRedactor {
    pub(crate) fn new(lease: Option<CredentialLease>) -> Self {
        Self(lease)
    }
    pub(crate) fn text(&self, text: &str) -> String {
        // This string has already been decoded from JSON. Encode it before
        // using the raw-body redactor so ordinary backslashes remain valid.
        let encoded = serde_json::to_vec(text).unwrap_or_default();
        let masked = match &self.0 {
            Some(lease) => lease.error_preview(&encoded),
            None => mask(&encoded, ""),
        };
        serde_json::from_str::<String>(masked.as_str())
            .unwrap_or_else(|_| masked.as_str().to_owned())
    }
    fn word(&self, value: &Value, limit: usize) -> Value {
        match value {
            Value::String(text) => {
                Value::String(self.text(text).as_str().chars().take(limit).collect())
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
            _ => Value::String("[invalid field shape]".into()),
        }
    }
    fn fields(&self, value: &Value, keys: &[&str]) -> Value {
        if !value.is_object() {
            return self.word(value, 64);
        }
        Value::Object(
            keys.iter()
                .filter_map(|key| {
                    value.get(*key).map(|value| {
                        (
                            (*key).into(),
                            self.word(value, if *key == "message" { 256 } else { 64 }),
                        )
                    })
                })
                .collect(),
        )
    }
    fn usage(&self, value: &Value) -> Value {
        let bytes = serde_json::to_vec(value).unwrap_or_default();
        if bytes.len() > super::error_text::ERROR_TEXT_BYTES {
            return serde_json::json!({"diagnostic":"usage exceeded diagnostic bound", "truncated":true});
        }
        let masked = match &self.0 {
            Some(lease) => lease.error_preview(&bytes),
            None => mask(&bytes, ""),
        };
        serde_json::from_str(masked.as_str()).unwrap_or_else(|_| {
            serde_json::json!({"diagnostic":"usage unavailable after masking", "truncated":true})
        })
    }

    pub(crate) fn usage_detail(&self, payload: &str, paths: &[&str]) -> MaskedTerminalFrame {
        let value = serde_json::from_str::<Value>(payload).ok();
        let usage = value
            .as_ref()
            .and_then(|value| paths.iter().find_map(|path| value.pointer(path)))
            .map_or_else(
                || serde_json::json!({"diagnostic":"usage unavailable"}),
                |usage| self.usage(usage),
            );
        MaskedTerminalFrame(serde_json::json!({"usage":usage}).to_string())
    }
    pub(crate) fn detail(&self, status: u16, payload: Option<&str>) -> ProviderErrorDetail {
        let mut frame = Map::new();
        if let Some(value) = payload.and_then(|text| serde_json::from_str::<Value>(text).ok()) {
            if let Some(kind) = value.get("type") {
                frame.insert("type".into(), self.word(kind, 64));
            }
            let response = value
                .get("response")
                .or_else(|| value.get("message"))
                .unwrap_or(&value);
            for key in ["status", "error", "incomplete_details", "usage"] {
                if let Some(field) = response.get(key).or_else(|| value.get(key)) {
                    let selected = match key {
                        "error" => self.fields(field, &["type", "code", "message"]),
                        "incomplete_details" => self.fields(field, &["reason"]),
                        "usage" => self.usage(field),
                        _ => self.word(field, 64),
                    };
                    frame.insert(key.into(), selected);
                }
            }
        } else {
            frame.insert("terminal_frame".into(), Value::String("unavailable".into()));
        }
        let mut text = Value::Object(frame).to_string();
        if text.len() > TERMINAL_DETAIL_BYTES {
            text = r#"{"terminal_frame":"exceeded diagnostic bound"}"#.into();
        }
        ProviderErrorDetail::terminal(status, MaskedTerminalFrame(text))
    }
}
