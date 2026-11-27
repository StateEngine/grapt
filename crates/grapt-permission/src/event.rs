//! Strict decode of durable permission decisions and rule withdrawals.
//!
//! Closed vocabularies: replies are the durable v1 union
//! `once | always | session | reject` (clone observes `always`×39, `once`×2;
//! `session`/`reject` are legal per the frozen schema). Suspension reasons
//! are `process-loss | shutdown` (contract; clone observes `process-loss`×10).
//! Cancel reasons observe `interrupted`. Unknown strings REJECT — no
//! defaulting, no forward-compat guessing on durable data.
//!
//! Null-vs-absent law: an explicit JSON `null` where a field is expected is a
//! decode error, distinct from absence (crate-wide policy; D1 proved zero
//! real events carry explicit null).

use grapt_types::{
    AgentId, AuthenticatedClientId, DecisionNonce, PermissionId, PermissionReply,
    PermissionSuspensionReason, ProjectId, RequestId, SessionId,
};
use serde_json::Value;
use std::str::FromStr;
mod revocation;
pub use revocation::{PermissionRuleRevocation, MAX_RULE_ORIGINS};

/// The wire spelling of a canonical [`PermissionReply`].
///
/// The crate-local `PermissionReply` enum that used to live here was a second
/// owner of a vocabulary `grapt-types` already owns. It is deleted; only the
/// oracle's wire spelling is crate business, and that is a projection of the
/// canonical value, not a competing definition of it.
pub fn reply_as_str(reply: PermissionReply) -> &'static str {
    match reply {
        PermissionReply::Once => "once",
        PermissionReply::Always => "always",
        PermissionReply::Session => "session",
        PermissionReply::Reject => "reject",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CancelReason {
    Interrupted,
    ApprovalUnavailable,
}

impl CancelReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::ApprovalUnavailable => "approval-unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionReplySource {
    DirectReply,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionReplyAttribution {
    pub principal: String,
    pub client: AuthenticatedClientId,
    pub request: RequestId,
    pub nonce: DecisionNonce,
    pub source: PermissionReplySource,
}

/// Every coordinate the HTTP permission projection needs to rebuild from a
/// `permission.asked.2` fact. Legacy v1 asks remain readable but deliberately
/// carry no synthetic HTTP authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionAskedCoordinates {
    pub agent: AgentId,
    pub project: ProjectId,
    pub resources: Vec<String>,
    pub save: Option<Vec<String>>,
    pub metadata: Option<Value>,
    pub source: Option<Value>,
    pub owner_subject: String,
    pub owner_client: AuthenticatedClientId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionEvent {
    RuleRevoked(PermissionRuleRevocation),
    Asked {
        id: PermissionId,
        session: SessionId,
        action: String,
        coordinates: Option<PermissionAskedCoordinates>,
    },
    Replied {
        request: PermissionId,
        session: SessionId,
        reply: PermissionReply,
        attribution: Option<PermissionReplyAttribution>,
    },
    Suspended {
        request: PermissionId,
        session: SessionId,
        reason: PermissionSuspensionReason,
    },
    Cancelled {
        request: PermissionId,
        session: SessionId,
        reason: CancelReason,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PermissionEventError {
    #[error("field {field} is missing")]
    Missing { field: &'static str },
    #[error("field {field} is explicit null (absent and null are distinct)")]
    ExplicitNull { field: &'static str },
    #[error("field {field} is not a string")]
    NotAString { field: &'static str },
    #[error("field {field} is not an array")]
    NotAnArray { field: &'static str },
    #[error("field {field} exceeds the {max}-value bound")]
    ValueBound { field: &'static str, max: usize },
    #[error("field {field}: {value:?} is outside the closed vocabulary")]
    OutsideVocabulary { field: &'static str, value: String },
    #[error("identity {field}: {source}")]
    Identity {
        field: &'static str,
        source: grapt_types::IdError,
    },
    #[error("unknown permission event kind {kind:?} version {version}")]
    UnknownKind { kind: String, version: u32 },
}

fn required_str<'v>(data: &'v Value, field: &'static str) -> Result<&'v str, PermissionEventError> {
    match data.get(field) {
        None => Err(PermissionEventError::Missing { field }),
        Some(Value::Null) => Err(PermissionEventError::ExplicitNull { field }),
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(PermissionEventError::NotAString { field }),
    }
}

fn identity<T: FromStr<Err = grapt_types::IdError>>(
    data: &Value,
    field: &'static str,
) -> Result<T, PermissionEventError> {
    required_str(data, field)?
        .parse()
        .map_err(|source| PermissionEventError::Identity { field, source })
}

fn nonempty_str(data: &Value, field: &'static str) -> Result<String, PermissionEventError> {
    let value = required_str(data, field)?;
    if value.is_empty() {
        return Err(PermissionEventError::OutsideVocabulary {
            field,
            value: String::new(),
        });
    }
    Ok(value.to_owned())
}

fn string_array(
    data: &Value,
    field: &'static str,
    max: usize,
) -> Result<Vec<String>, PermissionEventError> {
    let values = match data.get(field) {
        None => return Err(PermissionEventError::Missing { field }),
        Some(Value::Null) => return Err(PermissionEventError::ExplicitNull { field }),
        Some(Value::Array(values)) => values,
        Some(_) => return Err(PermissionEventError::NotAnArray { field }),
    };
    if values.len() > max {
        return Err(PermissionEventError::ValueBound { field, max });
    }
    values
        .iter()
        .map(|value| match value {
            Value::String(value) => Ok(value.clone()),
            _ => Err(PermissionEventError::NotAString { field }),
        })
        .collect()
}

fn optional_string_array(
    data: &Value,
    field: &'static str,
    max: usize,
) -> Result<Option<Vec<String>>, PermissionEventError> {
    match data.get(field) {
        Some(Value::Null) => Ok(None),
        Some(_) => string_array(data, field, max).map(Some),
        None => Err(PermissionEventError::Missing { field }),
    }
}

fn optional_value(
    data: &Value,
    field: &'static str,
) -> Result<Option<Value>, PermissionEventError> {
    match data.get(field) {
        Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(value.clone())),
        None => Err(PermissionEventError::Missing { field }),
    }
}

fn v2_asked(data: &Value) -> Result<PermissionAskedCoordinates, PermissionEventError> {
    let agent_raw = nonempty_str(data, "agentID")?;
    let agent = AgentId::new(agent_raw.clone()).ok_or(PermissionEventError::OutsideVocabulary {
        field: "agentID",
        value: agent_raw,
    })?;
    let project_raw = nonempty_str(data, "projectID")?;
    let project = ProjectId::new(project_raw.clone()).ok_or({
        PermissionEventError::OutsideVocabulary {
            field: "projectID",
            value: project_raw,
        }
    })?;
    let owner_client_raw = nonempty_str(data, "ownerClient")?;
    let owner_client = AuthenticatedClientId::new(owner_client_raw.clone()).ok_or({
        PermissionEventError::OutsideVocabulary {
            field: "ownerClient",
            value: owner_client_raw,
        }
    })?;
    Ok(PermissionAskedCoordinates {
        agent,
        project,
        resources: string_array(data, "resources", 128)?,
        save: optional_string_array(data, "save", 128)?,
        metadata: optional_value(data, "metadata")?,
        source: optional_value(data, "source")?,
        owner_subject: nonempty_str(data, "ownerSubject")?,
        owner_client,
    })
}

fn reply(data: &Value) -> Result<PermissionReply, PermissionEventError> {
    match required_str(data, "reply")? {
        "once" => Ok(PermissionReply::Once),
        "always" => Ok(PermissionReply::Always),
        "session" => Ok(PermissionReply::Session),
        "reject" => Ok(PermissionReply::Reject),
        other => Err(PermissionEventError::OutsideVocabulary {
            field: "reply",
            value: other.to_owned(),
        }),
    }
}

fn suspension_reason(data: &Value) -> Result<PermissionSuspensionReason, PermissionEventError> {
    match required_str(data, "reason")? {
        "process-loss" => Ok(PermissionSuspensionReason::ProcessLoss),
        "shutdown" => Ok(PermissionSuspensionReason::Shutdown),
        other => Err(PermissionEventError::OutsideVocabulary {
            field: "reason",
            value: other.to_owned(),
        }),
    }
}

fn cancel_reason(data: &Value) -> Result<CancelReason, PermissionEventError> {
    match required_str(data, "reason")? {
        "interrupted" => Ok(CancelReason::Interrupted),
        "approval-unavailable" => Ok(CancelReason::ApprovalUnavailable),
        other => Err(PermissionEventError::OutsideVocabulary {
            field: "reason",
            value: other.to_owned(),
        }),
    }
}

fn v2_attribution(data: &Value) -> Result<PermissionReplyAttribution, PermissionEventError> {
    let principal = required_str(data, "principal")?;
    if principal.is_empty() {
        return Err(PermissionEventError::OutsideVocabulary {
            field: "principal",
            value: principal.to_owned(),
        });
    }
    let client = AuthenticatedClientId::new(required_str(data, "clientID")?).ok_or_else(|| {
        PermissionEventError::OutsideVocabulary {
            field: "clientID",
            value: String::new(),
        }
    })?;
    let request = RequestId::new(required_str(data, "replyRequestID")?).ok_or_else(|| {
        PermissionEventError::OutsideVocabulary {
            field: "replyRequestID",
            value: String::new(),
        }
    })?;
    let nonce = DecisionNonce::new(required_str(data, "nonce")?).ok_or_else(|| {
        PermissionEventError::OutsideVocabulary {
            field: "nonce",
            value: String::new(),
        }
    })?;
    let source = match required_str(data, "source")? {
        "direct-reply" => PermissionReplySource::DirectReply,
        other => {
            return Err(PermissionEventError::OutsideVocabulary {
                field: "source",
                value: other.to_owned(),
            })
        }
    };
    Ok(PermissionReplyAttribution {
        principal: principal.to_owned(),
        client,
        request,
        nonce,
        source,
    })
}

impl PermissionEvent {
    /// Decode from a durable event's kind name, version, and data payload.
    /// Returns `Ok(None)` for non-permission kinds so callers can fold a
    /// mixed log without pre-filtering.
    pub fn decode(
        kind: &str,
        version: u32,
        data: &Value,
    ) -> Result<Option<Self>, PermissionEventError> {
        match (kind, version) {
            ("permission.rule.revoked", 1) => Ok(Some(Self::RuleRevoked(
                PermissionRuleRevocation::decode(data)?,
            ))),
            ("permission.asked", 1) => Ok(Some(PermissionEvent::Asked {
                id: identity(data, "id")?,
                session: identity(data, "sessionID")?,
                action: required_str(data, "action")?.to_owned(),
                coordinates: None,
            })),
            ("permission.asked", 2) => Ok(Some(PermissionEvent::Asked {
                id: identity(data, "id")?,
                session: identity(data, "sessionID")?,
                action: required_str(data, "action")?.to_owned(),
                coordinates: Some(v2_asked(data)?),
            })),
            ("permission.replied", 1) => Ok(Some(PermissionEvent::Replied {
                request: identity(data, "requestID")?,
                session: identity(data, "sessionID")?,
                reply: reply(data)?,
                attribution: None,
            })),
            ("permission.replied", 2) => Ok(Some(PermissionEvent::Replied {
                request: identity(data, "requestID")?,
                session: identity(data, "sessionID")?,
                reply: reply(data)?,
                attribution: Some(v2_attribution(data)?),
            })),
            ("permission.suspended", 1) => Ok(Some(PermissionEvent::Suspended {
                request: identity(data, "requestID")?,
                session: identity(data, "sessionID")?,
                reason: suspension_reason(data)?,
            })),
            ("permission.cancelled", 1) => Ok(Some(PermissionEvent::Cancelled {
                request: identity(data, "requestID")?,
                session: identity(data, "sessionID")?,
                reason: cancel_reason(data)?,
            })),
            (k, v) if k.starts_with("permission.") => Err(PermissionEventError::UnknownKind {
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
    fn decodes_the_real_shapes() -> Result<(), Box<dyn std::error::Error>> {
        let asked = PermissionEvent::decode(
            "permission.asked",
            1,
            &json!({"id":"per_a1","sessionID":"ses_b2","action":"external_directory",
                    "resources":["x"],"save":["x"],"source":"tool"}),
        )?
        .ok_or("permission.asked was not decoded")?;
        assert!(matches!(asked, PermissionEvent::Asked { .. }));

        let replied = PermissionEvent::decode(
            "permission.replied",
            1,
            &json!({"sessionID":"ses_b2","requestID":"per_a1","reply":"always"}),
        )?
        .ok_or("permission.replied was not decoded")?;
        assert!(matches!(
            replied,
            PermissionEvent::Replied {
                reply: PermissionReply::Always,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn explicit_null_is_not_absence() {
        let result = PermissionEvent::decode(
            "permission.replied",
            1,
            &json!({"sessionID":"ses_b2","requestID":"per_a1","reply":null}),
        );
        assert!(matches!(
            result,
            Err(PermissionEventError::ExplicitNull { field: "reply" })
        ));
    }

    #[test]
    fn vocabulary_is_closed() {
        let result = PermissionEvent::decode(
            "permission.replied",
            1,
            &json!({"sessionID":"ses_b2","requestID":"per_a1","reply":"maybe"}),
        );
        assert!(matches!(
            result,
            Err(PermissionEventError::OutsideVocabulary { field: "reply", .. })
        ));
    }

    #[test]
    fn non_permission_kinds_pass_through() -> Result<(), PermissionEventError> {
        assert_eq!(
            PermissionEvent::decode("session.created", 1, &json!({}))?,
            None
        );
        Ok(())
    }

    #[test]
    fn v2_reply_requires_and_decodes_the_complete_attribution_tuple(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let complete = json!({
            "sessionID": "ses_b2",
            "requestID": "per_a1",
            "reply": "once",
            "principal": "human-alpha",
            "clientID": "client-alpha",
            "replyRequestID": "req-alpha",
            "nonce": "nonce-alpha",
            "source": "direct-reply",
        });
        let decoded = PermissionEvent::decode("permission.replied", 2, &complete)?
            .ok_or("v2 permission reply was not decoded")?;
        let PermissionEvent::Replied {
            attribution: Some(attribution),
            ..
        } = decoded
        else {
            return Err("v2 reply lost attribution".into());
        };
        assert_eq!(attribution.principal, "human-alpha");
        assert_eq!(attribution.client.as_str(), "client-alpha");
        assert_eq!(attribution.request.as_str(), "req-alpha");
        assert_eq!(attribution.nonce.as_str(), "nonce-alpha");
        assert_eq!(attribution.source, PermissionReplySource::DirectReply);

        for field in ["principal", "clientID", "replyRequestID", "nonce", "source"] {
            let mut missing = complete.clone();
            let object = missing
                .as_object_mut()
                .ok_or("complete reply is an object")?;
            object.remove(field);
            assert!(
                matches!(
                    PermissionEvent::decode("permission.replied", 2, &missing),
                    Err(PermissionEventError::Missing { field: absent }) if absent == field
                ),
                "missing v2 attribution field {field} must refuse"
            );
        }
        Ok(())
    }

    #[test]
    fn v2_asked_requires_every_replay_authority_coordinate(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let complete = json!({
            "id": "per_replay",
            "sessionID": "ses_replay",
            "agentID": "agt_replay",
            "projectID": "prj_replay",
            "action": "write",
            "resources": ["/work/file"],
            "save": ["/work/*"],
            "metadata": {"reason": "test"},
            "source": {"type": "tool", "messageID": "msg-a", "callID": "call-a"},
            "ownerSubject": "human-replay",
            "ownerClient": "client-replay",
        });
        let decoded = PermissionEvent::decode("permission.asked", 2, &complete)?
            .ok_or("permission.asked.2 was not decoded")?;
        let PermissionEvent::Asked {
            coordinates: Some(coordinates),
            ..
        } = decoded
        else {
            return Err("v2 asked lost replay coordinates".into());
        };
        assert_eq!(coordinates.agent.as_str(), "agt_replay");
        assert_eq!(coordinates.project.as_str(), "prj_replay");
        assert_eq!(coordinates.owner_subject, "human-replay");
        assert_eq!(coordinates.owner_client.as_str(), "client-replay");

        for field in [
            "agentID",
            "projectID",
            "resources",
            "save",
            "metadata",
            "source",
            "ownerSubject",
            "ownerClient",
        ] {
            let mut missing = complete.clone();
            let object = missing
                .as_object_mut()
                .ok_or("asked payload is an object")?;
            object.remove(field);
            assert!(
                matches!(
                    PermissionEvent::decode("permission.asked", 2, &missing),
                    Err(PermissionEventError::Missing { field: absent }) if absent == field
                ),
                "missing replay coordinate {field} must refuse"
            );
        }
        Ok(())
    }

    #[test]
    fn corrupt_v2_attribution_is_not_defaulted() {
        let corrupt = json!({
            "sessionID": "ses_b2",
            "requestID": "per_a1",
            "reply": "once",
            "principal": "human-alpha",
            "clientID": "client-alpha",
            "replyRequestID": "req-alpha",
            "nonce": "nonce-alpha",
            "source": "replayed-as-authority",
        });
        assert!(
            matches!(
                PermissionEvent::decode("permission.replied", 2, &corrupt),
                Err(PermissionEventError::OutsideVocabulary {
                    field: "source",
                    ..
                })
            ),
            "corrupt v2 source must refuse"
        );
    }
}
