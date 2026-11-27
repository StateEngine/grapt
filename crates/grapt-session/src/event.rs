//! Strict decode of the session lifecycle event kinds this projection folds.
//! Other `session.*` kinds pass through as `Ok(None)` — they belong to the
//! tool/step, inbox, and execution clusters.

use grapt_types::{SessionId, SessionLaunch};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Created {
        session: SessionId,
        /// The full seed object, kept verbatim — downstream owners pick
        /// their fields; the projection reads slug/projectID/title/agent/
        /// model/time from it.
        info: Value,
    },
    Renamed {
        session: SessionId,
        title: String,
    },
    AgentSelected {
        session: SessionId,
        agent: String,
    },
    ModelSelected {
        session: SessionId,
        model: Value,
    },
    PresetSelected {
        session: SessionId,
        /// `NullOr` in the durable schema: an explicit null CLEARS the
        /// preset. This is the one place explicit null is contract-legal
        /// (D1: zero instances in real data, but the schema permits it).
        preset: Option<String>,
    },
    /// How this Session was launched, recorded once with its admission.
    ///
    /// A headless launch is consent to run with no window at all, so a window
    /// that later attaches and leaves must not stop it. This is a fact about
    /// the launch, not an inference from who is subscribed.
    Launched {
        session: SessionId,
        launch: SessionLaunch,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SessionEventError {
    #[error("field {field} is missing")]
    Missing { field: &'static str },
    #[error("field {field} has the wrong shape")]
    WrongShape { field: &'static str },
    #[error("identity {field}: {source}")]
    Identity {
        field: &'static str,
        source: grapt_types::IdError,
    },
}

fn session_id(data: &Value) -> Result<SessionId, SessionEventError> {
    match data.get("sessionID") {
        Some(Value::String(s)) => s.parse().map_err(|source| SessionEventError::Identity {
            field: "sessionID",
            source,
        }),
        Some(_) => Err(SessionEventError::WrongShape { field: "sessionID" }),
        None => Err(SessionEventError::Missing { field: "sessionID" }),
    }
}

fn required_string(data: &Value, field: &'static str) -> Result<String, SessionEventError> {
    match data.get(field) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(SessionEventError::WrongShape { field }),
        None => Err(SessionEventError::Missing { field }),
    }
}

impl SessionEvent {
    pub fn decode(
        kind: &str,
        version: u32,
        data: &Value,
    ) -> Result<Option<Self>, SessionEventError> {
        match (kind, version) {
            ("session.created", 1) => Ok(Some(SessionEvent::Created {
                session: session_id(data)?,
                info: data
                    .get("info")
                    .cloned()
                    .ok_or(SessionEventError::Missing { field: "info" })?,
            })),
            ("session.renamed", 1) => Ok(Some(SessionEvent::Renamed {
                session: session_id(data)?,
                title: required_string(data, "title")?,
            })),
            ("session.agent.selected", 1) => Ok(Some(SessionEvent::AgentSelected {
                session: session_id(data)?,
                agent: required_string(data, "agent")?,
            })),
            ("session.model.selected", 1) => Ok(Some(SessionEvent::ModelSelected {
                session: session_id(data)?,
                model: data
                    .get("model")
                    .cloned()
                    .ok_or(SessionEventError::Missing { field: "model" })?,
            })),
            ("session.preset.selected", 1) => Ok(Some(SessionEvent::PresetSelected {
                session: session_id(data)?,
                preset: match data.get("preset") {
                    None => return Err(SessionEventError::Missing { field: "preset" }),
                    Some(Value::Null) => None, // contract-legal explicit clear
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(_) => return Err(SessionEventError::WrongShape { field: "preset" }),
                },
            })),
            ("session.launched", 1) => Ok(Some(SessionEvent::Launched {
                session: session_id(data)?,
                launch: match data.get("launch") {
                    None => return Err(SessionEventError::Missing { field: "launch" }),
                    // A closed vocabulary at the decode edge: an unknown mode is
                    // the wrong shape, never a silent `interactive`, so a future
                    // writer that invents a third mode fails here rather than
                    // stopping every Session it launched.
                    Some(Value::String(raw)) => SessionLaunch::parse(raw)
                        .ok_or(SessionEventError::WrongShape { field: "launch" })?,
                    Some(_) => return Err(SessionEventError::WrongShape { field: "launch" }),
                },
            })),
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preset_null_clears_but_absence_rejects() -> Result<(), Box<dyn std::error::Error>> {
        let cleared = SessionEvent::decode(
            "session.preset.selected",
            1,
            &json!({"sessionID":"ses_a","preset":null}),
        )?
        .ok_or("session.preset.selected was not decoded")?;
        assert!(matches!(
            cleared,
            SessionEvent::PresetSelected { preset: None, .. }
        ));

        let result =
            SessionEvent::decode("session.preset.selected", 1, &json!({"sessionID":"ses_a"}));
        assert!(matches!(
            result,
            Err(SessionEventError::Missing { field: "preset" })
        ));
        Ok(())
    }
}
