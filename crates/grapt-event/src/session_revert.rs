//! The existing `session.revert.committed.1` payload shared by its producer
//! and readers. The target is the user message removed from visible history.

use grapt_types::{MessageId, SessionId};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertCommittedFact {
    session: SessionId,
    before: MessageId,
}

impl RevertCommittedFact {
    pub fn new(session: SessionId, before: MessageId) -> Self {
        Self { session, before }
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn before(&self) -> &MessageId {
        &self.before
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadError {
    NotObject,
    Missing(&'static str),
    Null(&'static str),
    NotString(&'static str),
    InvalidIdentity(&'static str),
    UnknownField(String),
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("session.revert.committed.1: ")?;
        match self {
            Self::NotObject => formatter.write_str("payload must be an object"),
            Self::Missing(field) => write!(formatter, "{field} is missing"),
            Self::Null(field) => write!(formatter, "{field} is explicitly null"),
            Self::NotString(field) => write!(formatter, "{field} must be a string"),
            Self::InvalidIdentity(field) => write!(formatter, "{field} is not a valid identity"),
            Self::UnknownField(field) => {
                write!(formatter, "payload contains unknown field {field}")
            }
        }
    }
}

impl std::error::Error for PayloadError {}

pub fn encode(fact: &RevertCommittedFact) -> Value {
    serde_json::json!({"sessionID":fact.session.as_str(), "to":fact.before.as_str()})
}

pub fn decode(value: &Value) -> Result<RevertCommittedFact, PayloadError> {
    let root = value.as_object().ok_or(PayloadError::NotObject)?;
    if let Some(key) = root.keys().find(|key| *key != "sessionID" && *key != "to") {
        return Err(PayloadError::UnknownField(key.clone()));
    }
    let session = required_str(root, "sessionID")?
        .parse()
        .map_err(|_| PayloadError::InvalidIdentity("sessionID"))?;
    let before = required_str(root, "to")?
        .parse()
        .map_err(|_| PayloadError::InvalidIdentity("to"))?;
    Ok(RevertCommittedFact::new(session, before))
}

fn required_str<'a>(
    root: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, PayloadError> {
    match root.get(field) {
        None => Err(PayloadError::Missing(field)),
        Some(Value::Null) => Err(PayloadError::Null(field)),
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(PayloadError::NotString(field)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frozen_revert_shape_round_trips_without_new_fields() -> Result<(), PayloadError> {
        let value = serde_json::json!({"sessionID":"ses_rewind", "to":"msg_selected"});
        let fact = decode(&value)?;
        assert_eq!(encode(&fact), value);
        assert_eq!(fact.session().as_str(), "ses_rewind");
        assert_eq!(fact.before().as_str(), "msg_selected");
        Ok(())
    }

    #[test]
    fn malformed_or_extended_revert_facts_refuse() {
        for value in [
            Value::Null,
            serde_json::json!([]),
            serde_json::json!({"to":"msg_selected"}),
            serde_json::json!({"sessionID":"ses_rewind"}),
            serde_json::json!({"sessionID":null, "to":"msg_selected"}),
            serde_json::json!({"sessionID":"ses_rewind", "to":null}),
            serde_json::json!({"sessionID":42, "to":"msg_selected"}),
            serde_json::json!({"sessionID":"ses_rewind", "to":42}),
            serde_json::json!({"sessionID":"bad", "to":"msg_selected"}),
            serde_json::json!({"sessionID":"ses_rewind", "to":"bad"}),
            serde_json::json!({"sessionID":"ses_rewind", "to":"msg_selected", "after":true}),
        ] {
            assert!(
                decode(&value).is_err(),
                "accepted malformed revert: {value}"
            );
        }
    }

    #[test]
    fn missing_null_and_invalid_target_are_distinct() {
        assert_eq!(
            decode(&serde_json::json!({"sessionID":"ses_rewind"})),
            Err(PayloadError::Missing("to"))
        );
        assert_eq!(
            decode(&serde_json::json!({"sessionID":"ses_rewind", "to":null})),
            Err(PayloadError::Null("to"))
        );
        assert_eq!(
            decode(&serde_json::json!({"sessionID":"ses_rewind", "to":"bad"})),
            Err(PayloadError::InvalidIdentity("to"))
        );
    }
}
