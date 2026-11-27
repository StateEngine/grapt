//! Strict decode of the four durable form event kinds. Shapes are taken from
//! the frozen fixture `form-suspend-settle-after-replacement` (the oracle-true
//! durable log this seat captured), never an oracle reverse:
//!
//! - `form.created.1`   `{ form: { id, sessionID, title, fields }, sessionID }`
//! - `form.suspended.1` `{ sessionID, formID, reason }`
//! - `form.replied.1`   `{ id, sessionID, answer: { .. } }`   (answered)
//! - `form.cancelled.1` `{ id, sessionID }`
//!
//! Null-vs-absent is a decode error, distinct from absence (crate-wide
//! policy). Suspension reasons are the closed `shutdown | process-loss`
//! vocabulary; unknown strings REJECT.

use grapt_types::{FormId, FormSuspensionReason, SessionId};
use serde_json::Value;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormEvent {
    Created {
        id: FormId,
        session: SessionId,
        title: String,
    },
    Suspended {
        form: FormId,
        session: SessionId,
        reason: FormSuspensionReason,
    },
    /// An answered form. The answer payload is retained verbatim (a Form's
    /// answer is opaque structured data owned by the form's fields).
    Answered {
        form: FormId,
        session: SessionId,
        answer: Value,
    },
    Cancelled {
        form: FormId,
        session: SessionId,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FormEventError {
    #[error("field {field} is missing")]
    Missing { field: &'static str },
    #[error("field {field} is explicit null (absent and null are distinct)")]
    ExplicitNull { field: &'static str },
    #[error("field {field} is not a string")]
    NotAString { field: &'static str },
    #[error("field {field} is not an object")]
    NotAnObject { field: &'static str },
    #[error("suspension reason {value:?} outside the closed vocabulary")]
    UnknownReason { value: String },
    #[error("identity {field}: {source}")]
    Identity {
        field: &'static str,
        source: grapt_types::IdError,
    },
    #[error("unknown form event kind {kind:?} version {version}")]
    UnknownKind { kind: String, version: u32 },
}

fn required_str<'v>(data: &'v Value, field: &'static str) -> Result<&'v str, FormEventError> {
    match data.get(field) {
        None => Err(FormEventError::Missing { field }),
        Some(Value::Null) => Err(FormEventError::ExplicitNull { field }),
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(FormEventError::NotAString { field }),
    }
}

fn required_object<'v>(data: &'v Value, field: &'static str) -> Result<&'v Value, FormEventError> {
    match data.get(field) {
        None => Err(FormEventError::Missing { field }),
        Some(Value::Null) => Err(FormEventError::ExplicitNull { field }),
        Some(value @ Value::Object(_)) => Ok(value),
        Some(_) => Err(FormEventError::NotAnObject { field }),
    }
}

fn identity<T: FromStr<Err = grapt_types::IdError>>(
    value: &str,
    field: &'static str,
) -> Result<T, FormEventError> {
    value
        .parse()
        .map_err(|source| FormEventError::Identity { field, source })
}

fn reason(data: &Value) -> Result<FormSuspensionReason, FormEventError> {
    match required_str(data, "reason")? {
        "shutdown" => Ok(FormSuspensionReason::Shutdown),
        "process-loss" => Ok(FormSuspensionReason::ProcessLoss),
        other => Err(FormEventError::UnknownReason {
            value: other.to_owned(),
        }),
    }
}

impl FormEvent {
    /// Returns `Ok(None)` for non-form kinds so callers fold a mixed log.
    pub fn decode(kind: &str, version: u32, data: &Value) -> Result<Option<Self>, FormEventError> {
        match (kind, version) {
            ("form.created", 1) => {
                let form = required_object(data, "form")?;
                Ok(Some(FormEvent::Created {
                    id: identity(required_str(form, "id")?, "form.id")?,
                    session: identity(required_str(data, "sessionID")?, "sessionID")?,
                    title: required_str(form, "title")?.to_owned(),
                }))
            }
            ("form.suspended", 1) => Ok(Some(FormEvent::Suspended {
                form: identity(required_str(data, "formID")?, "formID")?,
                session: identity(required_str(data, "sessionID")?, "sessionID")?,
                reason: reason(data)?,
            })),
            ("form.replied", 1) => Ok(Some(FormEvent::Answered {
                form: identity(required_str(data, "id")?, "id")?,
                session: identity(required_str(data, "sessionID")?, "sessionID")?,
                answer: required_object(data, "answer")?.clone(),
            })),
            ("form.cancelled", 1) => Ok(Some(FormEvent::Cancelled {
                form: identity(required_str(data, "id")?, "id")?,
                session: identity(required_str(data, "sessionID")?, "sessionID")?,
            })),
            (k, v) if k.starts_with("form.") => Err(FormEventError::UnknownKind {
                kind: k.to_owned(),
                version: v,
            }),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_the_frozen_fixture_shapes() -> Result<(), Box<dyn std::error::Error>> {
        let created = FormEvent::decode(
            "form.created",
            1,
            &json!({"form":{"id":"frm_a","sessionID":"ses_b","title":"T",
                    "fields":[{"key":"name","required":true,"type":"string"}]},
                    "sessionID":"ses_b"}),
        )?
        .ok_or("created")?;
        assert!(matches!(created, FormEvent::Created { .. }));

        let suspended = FormEvent::decode(
            "form.suspended",
            1,
            &json!({"sessionID":"ses_b","formID":"frm_a","reason":"shutdown"}),
        )?
        .ok_or("suspended")?;
        assert!(matches!(
            suspended,
            FormEvent::Suspended {
                reason: FormSuspensionReason::Shutdown,
                ..
            }
        ));

        let answered = FormEvent::decode(
            "form.replied",
            1,
            &json!({"id":"frm_a","sessionID":"ses_b","answer":{"name":"Ada"}}),
        )?
        .ok_or("answered")?;
        assert!(matches!(answered, FormEvent::Answered { .. }));

        let cancelled = FormEvent::decode(
            "form.cancelled",
            1,
            &json!({"id":"frm_a","sessionID":"ses_b"}),
        )?
        .ok_or("cancelled")?;
        assert!(matches!(cancelled, FormEvent::Cancelled { .. }));
        Ok(())
    }

    #[test]
    fn reason_vocabulary_is_closed() {
        let result = FormEvent::decode(
            "form.suspended",
            1,
            &json!({"sessionID":"ses_b","formID":"frm_a","reason":"paused"}),
        );
        assert_eq!(
            result,
            Err(FormEventError::UnknownReason {
                value: "paused".to_owned()
            })
        );
    }

    #[test]
    fn explicit_null_answer_is_not_absence() {
        let result = FormEvent::decode(
            "form.replied",
            1,
            &json!({"id":"frm_a","sessionID":"ses_b","answer":null}),
        );
        assert_eq!(
            result,
            Err(FormEventError::ExplicitNull { field: "answer" })
        );
    }

    #[test]
    fn non_form_kinds_pass_through() -> Result<(), FormEventError> {
        assert_eq!(FormEvent::decode("session.created", 1, &json!({}))?, None);
        Ok(())
    }
}
