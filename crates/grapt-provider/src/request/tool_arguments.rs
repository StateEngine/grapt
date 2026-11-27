//! History tool-call arguments in the shape every provider accepts.
//!
//! A model can emit call arguments that are not a JSON object, for example when
//! its output is cut off mid-string. That call already failed as a tool error,
//! and history keeps its text as emitted. Providers parse replayed arguments and
//! refuse the whole request when they are not an object, so the routes project
//! such arguments as a marker object instead. The marker tells the model why
//! its earlier call failed, and the session can continue.

use super::CodexToolText;
use serde_json::Value;

/// Sent in place of history arguments that are not a JSON object.
const INVALID_ARGUMENTS: &str = r#"{"_invalid_arguments_truncated":true}"#;

impl CodexToolText {
    /// The arguments as a JSON object, or `None` when they are not one.
    pub(crate) fn object(&self) -> Option<Value> {
        serde_json::from_str(self.as_str())
            .ok()
            .filter(Value::is_object)
    }

    /// The arguments as JSON object text: unchanged when they are one,
    /// otherwise [`INVALID_ARGUMENTS`].
    pub(crate) fn object_text(&self) -> &str {
        if self.object().is_some() {
            self.as_str()
        } else {
            INVALID_ARGUMENTS
        }
    }

    /// The arguments as a JSON object, or the [`INVALID_ARGUMENTS`] marker.
    pub(crate) fn object_value(&self) -> Value {
        self.object().unwrap_or_else(invalid_value)
    }
}

/// [`INVALID_ARGUMENTS`] as a value.
fn invalid_value() -> Value {
    serde_json::json!({ "_invalid_arguments_truncated": true })
}
