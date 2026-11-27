//! Strict decode of the two durable admission event kinds.

use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, DeliveryClaimId, DeliveryMode, MessageId, OperationId,
    RuntimeGeneration, SessionId, SubagentOperationId,
};
use serde_json::Value;
use std::str::FromStr;
pub(crate) mod native_mail;

/// Durable settlement carried by an operation-delivery fact. Completion owns
/// a verified artifact; failure and quarantine are explicit parent-visible
/// debts and therefore cannot masquerade as completed output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationDeliverySettlement {
    Completed,
    Failed,
    Quarantined,
}

impl OperationDeliverySettlement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Quarantined => "quarantined",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "quarantined" => Some(Self::Quarantined),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxEvent {
    /// Mail is captured with a provider send permit, never promoted as work.
    AgentMessageAdmitted(grapt_store::agent_message::AgentMessage),
    Admitted {
        session: SessionId,
        input: MessageId,
        delivery: DeliveryMode,
    },
    /// A synthetic input admitted by the Rust authority. Historical oracle
    /// rows lack `rustAuthorized: true` and remain audit-only below.
    SyntheticAdmitted {
        session: SessionId,
        input: MessageId,
        delivery: DeliveryMode,
    },
    Promoted {
        session: SessionId,
        input: MessageId,
    },
    OperationDeliveryAdmitted {
        session: SessionId,
        input: MessageId,
        operation: OperationId,
        subagent: SubagentOperationId,
        claim: DeliveryClaimId,
        generation: RuntimeGeneration,
        settlement: OperationDeliverySettlement,
        artifact: Option<ArtifactRef>,
        projection: String,
    },
    /// Historical v1 `type: synthetic` rows are preserved for audit/migration
    /// only. They never enter the active pending fold.
    LegacySyntheticAdmitted {
        session: SessionId,
        input: MessageId,
    },
    /// A user-requested compaction, pending until its manual terminal. At most
    /// one is pending per Session; it runs inside the next promoted input's
    /// first Step.
    CompactionAdmitted {
        session: SessionId,
        input: MessageId,
    },
    /// A manual compaction reached its terminal (`ended` or `failed`), which
    /// settles the Session's pending compaction. Automatic compactions never
    /// decode to this.
    CompactionSettled { session: SessionId },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum InboxEventError {
    #[error("compaction instructions must contain text and fit within 16 KiB")]
    InvalidCompactionInstructions,
    #[error("native mail state: {0}")]
    NativeMailState(String),
    #[error("agent message: {0}")]
    AgentMessage(String),
    #[error("field {field} is missing")]
    Missing { field: &'static str },
    #[error("field {field} is explicit null (absent and null are distinct)")]
    ExplicitNull { field: &'static str },
    #[error("field {field} is not a string")]
    NotAString { field: &'static str },
    #[error("delivery {value:?} is outside the closed vocabulary")]
    UnknownDelivery { value: String },
    #[error("input type {value:?} is outside the closed vocabulary")]
    UnknownInputType { value: String },
    #[error("operation delivery settlement {value:?} is outside the closed vocabulary")]
    UnknownSettlement { value: String },
    #[error("operation delivery settlement {settlement:?} has an invalid artifact shape")]
    InvalidSettlementArtifact {
        settlement: OperationDeliverySettlement,
    },
    #[error("field {field} is not a non-negative integer")]
    NotAnInteger { field: &'static str },
    #[error("identity {field}: {source}")]
    Identity {
        field: &'static str,
        source: grapt_types::IdError,
    },
    #[error("identity {field} is empty")]
    EmptyIdentity { field: &'static str },
    #[error("unknown inbox event kind {kind:?} version {version}")]
    UnknownKind { kind: String, version: u32 },
}

fn required_str<'v>(data: &'v Value, field: &'static str) -> Result<&'v str, InboxEventError> {
    match data.get(field) {
        None => Err(InboxEventError::Missing { field }),
        Some(Value::Null) => Err(InboxEventError::ExplicitNull { field }),
        Some(Value::String(s)) => Ok(s),
        Some(_) => Err(InboxEventError::NotAString { field }),
    }
}

fn identity<T: FromStr<Err = grapt_types::IdError>>(
    data: &Value,
    field: &'static str,
) -> Result<T, InboxEventError> {
    required_str(data, field)?
        .parse()
        .map_err(|source| InboxEventError::Identity { field, source })
}

fn decode_operation_delivery(
    data: &Value,
    input_obj: &Value,
) -> Result<InboxEvent, InboxEventError> {
    let generation = input_obj.get("generation").and_then(Value::as_u64).ok_or(
        InboxEventError::NotAnInteger {
            field: "generation",
        },
    )?;
    let settlement_raw = required_str(input_obj, "settlement")?;
    let settlement = OperationDeliverySettlement::from_str(settlement_raw).ok_or_else(|| {
        InboxEventError::UnknownSettlement {
            value: settlement_raw.to_owned(),
        }
    })?;
    let artifact = match (settlement, input_obj.get("artifact")) {
        (OperationDeliverySettlement::Completed, Some(artifact_obj)) => {
            let length = artifact_obj.get("length").and_then(Value::as_u64).ok_or(
                InboxEventError::NotAnInteger {
                    field: "artifact.length",
                },
            )?;
            let artifact_id = ArtifactId::new(required_str(artifact_obj, "id")?).ok_or(
                InboxEventError::EmptyIdentity {
                    field: "artifact.id",
                },
            )?;
            let artifact_digest = ArtifactDigest::new(required_str(artifact_obj, "digest")?)
                .ok_or(InboxEventError::EmptyIdentity {
                    field: "artifact.digest",
                })?;
            Some(ArtifactRef::new(artifact_id, length, artifact_digest))
        }
        (OperationDeliverySettlement::Completed, None)
        | (OperationDeliverySettlement::Failed, Some(_))
        | (OperationDeliverySettlement::Quarantined, Some(_)) => {
            return Err(InboxEventError::InvalidSettlementArtifact { settlement });
        }
        (OperationDeliverySettlement::Failed, None)
        | (OperationDeliverySettlement::Quarantined, None) => None,
    };
    Ok(InboxEvent::OperationDeliveryAdmitted {
        session: identity(data, "sessionID")?,
        input: identity(data, "inputID")?,
        operation: OperationId::new(required_str(input_obj, "operationID")?).ok_or(
            InboxEventError::EmptyIdentity {
                field: "operationID",
            },
        )?,
        subagent: SubagentOperationId::new(required_str(input_obj, "subagentOperationID")?).ok_or(
            InboxEventError::EmptyIdentity {
                field: "subagentOperationID",
            },
        )?,
        claim: DeliveryClaimId::new(required_str(input_obj, "claimID")?)
            .ok_or(InboxEventError::EmptyIdentity { field: "claimID" })?,
        generation: RuntimeGeneration::new(generation),
        settlement,
        artifact,
        projection: required_str(input_obj, "projection")?.to_owned(),
    })
}

impl InboxEvent {
    /// Returns `Ok(None)` for control facts and non-inbox kinds so callers fold a mixed log.
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn decode(kind: &str, version: u32, data: &Value) -> Result<Option<Self>, InboxEventError> {
        match (kind, version) {
            ("session.input.admitted", 1) => {
                let input_obj = data
                    .get("input")
                    .ok_or(InboxEventError::Missing { field: "input" })?;
                let input_type = required_str(input_obj, "type")?;
                if input_type == "agent-message" {
                    return grapt_store::agent_message::AgentMessage::decode(data)
                        .map(InboxEvent::AgentMessageAdmitted)
                        .map(Some)
                        .map_err(InboxEventError::AgentMessage);
                }
                if input_type == "operation-delivery" {
                    return decode_operation_delivery(data, input_obj).map(Some);
                }
                if input_type == "synthetic" {
                    if input_obj.get("rustAuthorized").and_then(Value::as_bool) == Some(true) {
                        let delivery = match required_str(input_obj, "delivery")? {
                            "steer" => DeliveryMode::Steer,
                            "queue" => DeliveryMode::Queue,
                            other => {
                                return Err(InboxEventError::UnknownDelivery {
                                    value: other.to_owned(),
                                })
                            }
                        };
                        return Ok(Some(InboxEvent::SyntheticAdmitted {
                            session: identity(data, "sessionID")?,
                            input: identity(data, "inputID")?,
                            delivery,
                        }));
                    }
                    return Ok(Some(InboxEvent::LegacySyntheticAdmitted {
                        session: identity(data, "sessionID")?,
                        input: identity(data, "inputID")?,
                    }));
                }
                if input_type != "user" {
                    return Err(InboxEventError::UnknownInputType {
                        value: input_type.to_owned(),
                    });
                }
                let delivery = match required_str(input_obj, "delivery")? {
                    "steer" => DeliveryMode::Steer,
                    "queue" => DeliveryMode::Queue,
                    other => {
                        return Err(InboxEventError::UnknownDelivery {
                            value: other.to_owned(),
                        })
                    }
                };
                Ok(Some(InboxEvent::Admitted {
                    session: identity(data, "sessionID")?,
                    input: identity(data, "inputID")?,
                    delivery,
                }))
            }
            ("session.native-mail-state", 1) => {
                native_mail::validate_state(data)
                    .map_err(|reason| InboxEventError::NativeMailState(reason.into()))?;
                Ok(None)
            }
            ("session.input.native-wake-held", 1) => {
                // The hold controls automatic wake recovery, not the pending
                // fold. Validate its coordinates without consuming the input.
                let _session: SessionId = identity(data, "sessionID")?;
                let _input: MessageId = identity(data, "inputID")?;
                Ok(None)
            }
            ("session.input.promoted", 1) => Ok(Some(InboxEvent::Promoted {
                session: identity(data, "sessionID")?,
                input: identity(data, "inputID")?,
            })),
            ("session.compaction.admitted", 1) => {
                compaction_instructions(data)?;
                Ok(Some(InboxEvent::CompactionAdmitted {
                    session: identity(data, "sessionID")?,
                    input: identity(data, "inputID")?,
                }))
            }
            // The oracle's terminals name the manual reason; the Rust-native
            // descriptor names the trigger that produced it.
            ("session.compaction.ended" | "session.compaction.failed", 1) => {
                if required_str(data, "reason")? != "manual" {
                    return Ok(None);
                }
                Ok(Some(InboxEvent::CompactionSettled {
                    session: identity(data, "sessionID")?,
                }))
            }
            ("session.compaction.ended", 2) => {
                if required_str(data, "trigger")? != "userRequested" {
                    return Ok(None);
                }
                Ok(Some(InboxEvent::CompactionSettled {
                    session: identity(data, "sourceSession")?,
                }))
            }
            (k, v) if k.starts_with("session.input.") || k == "session.native-mail-state" => {
                Err(InboxEventError::UnknownKind {
                    kind: k.to_owned(),
                    version: v,
                })
            }
            _ => Ok(None),
        }
    }
}

/// Optional manual guidance is part of the admission event, never ambient state.
pub(crate) fn compaction_instructions(data: &Value) -> Result<Option<&str>, InboxEventError> {
    if data.get("instructions").is_none() {
        return Ok(None);
    }
    let text = required_str(data, "instructions")?;
    if text.trim().is_empty() || text.len() > 16 * 1024 {
        return Err(InboxEventError::InvalidCompactionInstructions);
    }
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_real_shapes() -> Result<(), Box<dyn std::error::Error>> {
        let admitted = InboxEvent::decode(
            "session.input.admitted",
            1,
            &json!({"sessionID":"ses_a","inputID":"msg_b",
                    "input":{"type":"user","delivery":"steer","data":{"text":"hi"}}}),
        )?
        .ok_or("admitted event")?;
        assert!(matches!(
            admitted,
            InboxEvent::Admitted {
                delivery: DeliveryMode::Steer,
                ..
            }
        ));

        let promoted = InboxEvent::decode(
            "session.input.promoted",
            1,
            &json!({"sessionID":"ses_a","inputID":"msg_b"}),
        )?
        .ok_or("promoted event")?;
        assert!(matches!(promoted, InboxEvent::Promoted { .. }));
        Ok(())
    }

    #[test]
    fn delivery_vocabulary_is_closed() {
        let result = InboxEvent::decode(
            "session.input.admitted",
            1,
            &json!({"sessionID":"ses_a","inputID":"msg_b",
                    "input":{"type":"user","delivery":"broadcast"}}),
        );
        assert_eq!(
            result,
            Err(InboxEventError::UnknownDelivery {
                value: "broadcast".to_owned()
            })
        );
    }
}
