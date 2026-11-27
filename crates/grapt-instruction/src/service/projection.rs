use super::history::{
    apply_history_event, check_cache, fold_history, fold_parent_history, load_fork,
    write_folded_state,
};
use super::InstructionError;
use crate::{InstructionDeltaError, InstructionFold};
use grapt_store::{DecodedEvent, ProjectionId, Projector};
use grapt_types::{
    EventSeq, ForkBoundary, InstructionHash, InstructionKey, MessageId, ProjectId, SessionId,
    WorkspaceId,
};
use rusqlite::Transaction;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Default)]
pub struct InstructionProjector;

impl Projector for InstructionProjector {
    fn projection(&self) -> ProjectionId {
        ProjectionId::new("instruction-state")
    }

    fn project(
        &mut self,
        tx: &Transaction<'_>,
        event: &DecodedEvent,
    ) -> Result<(), grapt_store::ProjectorError> {
        project_event(tx, event).map_err(|error| match error {
            InstructionError::Sqlite(error) => error.into(),
            error => error.to_string().into(),
        })
    }
}

pub(super) fn decode_event_delta(
    session: &SessionId,
    data: &Value,
) -> Result<BTreeMap<InstructionKey, Option<InstructionHash>>, InstructionError> {
    let Value::Object(object) = data else {
        return Err(InstructionError::Corruption {
            reason: "instruction event data is not an object".into(),
        });
    };
    let event_session = object
        .get("sessionID")
        .and_then(Value::as_str)
        .ok_or_else(|| InstructionError::Corruption {
            reason: "instruction event sessionID is missing or not a string".into(),
        })?;
    if event_session != session.as_str() {
        return Err(InstructionError::Corruption {
            reason: "instruction event sessionID differs from its aggregate".into(),
        });
    }
    let delta = object
        .get("delta")
        .ok_or_else(|| InstructionError::Corruption {
            reason: "instruction event delta is missing".into(),
        })?;
    let mut fold = InstructionFold::default();
    fold.apply(session.clone(), delta)?;
    let Value::Object(map) = delta else {
        return Err(InstructionError::Delta(InstructionDeltaError::NotObject));
    };
    map.iter()
        .map(|(key, hash)| {
            let key = InstructionKey::new(key).ok_or(InstructionDeltaError::EmptyKey)?;
            let Value::String(hash) = hash else {
                return Err(InstructionDeltaError::NonStringValue {
                    key: key.to_string(),
                }
                .into());
            };
            if hash == "removed" {
                return Ok((key, None));
            }
            let parsed = hash
                .parse()
                .map_err(|source| InstructionDeltaError::InvalidHash {
                    key: key.to_string(),
                    source,
                })?;
            Ok((key, Some(parsed)))
        })
        .collect()
}

fn project_event(tx: &Transaction<'_>, event: &DecodedEvent) -> Result<(), InstructionError> {
    match (event.kind.name(), event.kind.version().value()) {
        ("session.instructions.updated", 2)
        | ("session.compaction.ended", 1 | 2)
        | ("session.moved", 1)
        | ("session.revert.committed", 1) => {
            ensure_exact_instruction_event(event)?;
            let mut fold = fold_history(tx, &event.aggregate_id)?;
            check_cache(tx, &event.aggregate_id, &fold)?;
            // Replay, request projection and commit use the same aggregate
            // transition, including a child's restarted instruction epoch.
            apply_history_event(tx, &mut fold.history, event)?;
            write_folded_state(tx, &event.aggregate_id, fold.persistable())
        }
        ("session.forked", 2) => project_fork(tx, event),
        ("session.instructions.updated", version)
        | ("session.compaction.ended", version)
        | ("session.moved", version)
        | ("session.revert.committed", version)
        | ("session.forked", version) => Err(InstructionError::UnsupportedVersion {
            kind: event.kind.name().to_owned(),
            version,
        }),
        _ => Ok(()),
    }
}

/// This crate writes `session.instructions.updated`, so an append carries
/// exactly sessionID and delta; reads tolerate fields a later writer adds.
fn ensure_exact_instruction_event(event: &DecodedEvent) -> Result<(), InstructionError> {
    if event.kind.name() != "session.instructions.updated" {
        return Ok(());
    }
    match &event.data {
        Value::Object(object)
            if object.len() == 2
                && object.contains_key("sessionID")
                && object.contains_key("delta") =>
        {
            Ok(())
        }
        _ => Err(InstructionError::Corruption {
            reason: "instruction event must contain exactly sessionID and delta".into(),
        }),
    }
}

fn project_fork(tx: &Transaction<'_>, event: &DecodedEvent) -> Result<(), InstructionError> {
    let fork = decode_fork(event)?;
    if event.seq != EventSeq::FIRST {
        return Err(InstructionError::Corruption {
            reason: "session.forked must be the child's first aggregate event".into(),
        });
    }
    if load_fork(tx, &event.aggregate_id)?.is_some() {
        return Err(InstructionError::Corruption {
            reason: format!(
                "session {} already has a durable fork fact",
                event.aggregate_id
            ),
        });
    }
    if fold_history(tx, &event.aggregate_id)?.history.is_some() {
        return Err(InstructionError::Corruption {
            reason: format!(
                "session {} has instruction history before its fork fact",
                event.aggregate_id
            ),
        });
    }
    let inherited = fold_parent_history(tx, &fork)?;
    check_cache(tx, &event.aggregate_id, &inherited)?;
    write_folded_state(tx, &event.aggregate_id, inherited.persistable())
}

pub(super) fn decode_fork(event: &DecodedEvent) -> Result<ForkBoundary, InstructionError> {
    // Frozen `SessionEvent.Forked` v2 is the ancestry authority. `parentSeq`
    // caps inherited aggregate history; the optional message only identifies
    // the product boundary and does not replace that durable sequence.
    if event.kind.name() != "session.forked" || event.kind.version().value() != 2 {
        return Err(InstructionError::UnsupportedVersion {
            kind: event.kind.name().to_owned(),
            version: event.kind.version().value(),
        });
    }
    let object = required_object(
        &event.data,
        "session.forked",
        &["sessionID", "parentID", "parentSeq"],
    )?;
    decode_session(event, object)?;
    let parent = parse_id::<SessionId>(
        required_string(object, "parentID", "session.forked")?,
        "parentID",
    )?;
    // `parentSeq` must still be an integer here: a JSON string or float is a
    // decode fault, not a boundary-law fault, and the promoted type only ever
    // sees a real i64.
    let raw_parent_seq = object
        .get("parentSeq")
        .and_then(Value::as_i64)
        .ok_or_else(|| InstructionError::Corruption {
            reason: "session.forked parentSeq must be an integer at least -1".into(),
        })?;
    if let Some(from) = object.get("from") {
        parse_id::<MessageId>(
            from.as_str().ok_or_else(|| InstructionError::Corruption {
                reason: "session.forked from must be a message ID string".into(),
            })?,
            "from",
        )?;
    }
    // Self-parent and the `-1` floor are the promoted law. This crate no
    // longer restates either check; it cannot hold an invalid boundary.
    Ok(ForkBoundary::from_raw_parent_seq(
        event.aggregate_id.clone(),
        parent,
        raw_parent_seq,
    )?)
}

pub(super) fn decode_compaction_ended(event: &DecodedEvent) -> Result<(), InstructionError> {
    // Every field of the frozen v1 boundary is required. Accepting only
    // sessionID would let a corrupt foreign event advance or clear the
    // instruction epoch.
    let object = required_object(
        &event.data,
        "session.compaction.ended",
        &["sessionID", "reason", "text", "recent"],
    )?;
    decode_session(event, object)?;
    match required_string(object, "reason", "session.compaction.ended")? {
        "auto" | "manual" => {}
        _ => {
            return Err(InstructionError::Corruption {
                reason: "session.compaction.ended reason must be auto or manual".into(),
            });
        }
    }
    required_string(object, "text", "session.compaction.ended")?;
    required_string(object, "recent", "session.compaction.ended")?;
    Ok(())
}

/// Validate the Rust-native v2 compaction boundary.
///
/// The descriptor's shape is NOT restated here. It is owned by
/// `grapt_event::session_compaction`, and this crate calls that one strict
/// codec — a second copy of an eleven-coordinate contract is a second thing to
/// drift, and the fold does not need to understand the descriptor to move an
/// epoch. What this crate still owns is the same check v1 makes: the fact must
/// name the aggregate it rides, or a corrupt foreign event could advance or
/// clear this session's instruction epoch.
pub(super) fn decode_compaction_ended_v2(event: &DecodedEvent) -> Result<(), InstructionError> {
    let descriptor = grapt_event::session_compaction::decode(&event.data).map_err(|source| {
        InstructionError::Corruption {
            reason: format!("session.compaction.ended.2 payload is invalid: {source}"),
        }
    })?;
    if descriptor.source_session() == &event.aggregate_id {
        return Ok(());
    }
    Err(InstructionError::Corruption {
        reason: "session.compaction.ended.2 sourceSession differs from its aggregate".into(),
    })
}

pub(super) fn decode_moved(event: &DecodedEvent) -> Result<(), InstructionError> {
    let object = required_object(&event.data, "session.moved", &["sessionID", "location"])?;
    decode_session(event, object)?;
    let location = required_object(
        object
            .get("location")
            .ok_or_else(|| InstructionError::Corruption {
                reason: "session.moved location is missing".into(),
            })?,
        "session.moved location",
        &["directory"],
    )?;
    required_string(location, "directory", "session.moved location")?;
    if let Some(workspace) = location.get("workspaceID") {
        WorkspaceId::new(
            workspace
                .as_str()
                .ok_or_else(|| InstructionError::Corruption {
                    reason: "session.moved workspaceID must be a string".into(),
                })?,
        )
        .ok_or_else(|| InstructionError::Corruption {
            reason: "session.moved workspaceID cannot be empty".into(),
        })?;
    }
    if let Some(project) = object.get("projectID") {
        ProjectId::new(
            project
                .as_str()
                .ok_or_else(|| InstructionError::Corruption {
                    reason: "session.moved projectID must be a string".into(),
                })?,
        )
        .ok_or_else(|| InstructionError::Corruption {
            reason: "session.moved projectID cannot be empty".into(),
        })?;
    }
    if object
        .get("subpath")
        .is_some_and(|value| !value.is_string())
    {
        return Err(InstructionError::Corruption {
            reason: "session.moved subpath must be a string".into(),
        });
    }
    Ok(())
}

pub(super) fn decode_revert_committed(event: &DecodedEvent) -> Result<(), InstructionError> {
    let fact = grapt_event::session_revert::decode(&event.data).map_err(|error| {
        InstructionError::Corruption {
            reason: error.to_string(),
        }
    })?;
    if fact.session() != &event.aggregate_id {
        return Err(InstructionError::Corruption {
            reason: "session.revert.committed sessionID differs from its aggregate".into(),
        });
    }
    Ok(())
}

fn decode_session(
    event: &DecodedEvent,
    object: &serde_json::Map<String, Value>,
) -> Result<(), InstructionError> {
    let session = parse_id::<SessionId>(
        required_string(object, "sessionID", event.kind.name())?,
        "sessionID",
    )?;
    if session == event.aggregate_id {
        return Ok(());
    }
    Err(InstructionError::Corruption {
        reason: format!("{} sessionID differs from its aggregate", event.kind.name()),
    })
}

/// An object carrying every `required` field. Fields this build does not
/// know are tolerated, so a row a newer or older writer left still reads.
fn required_object<'a>(
    value: &'a Value,
    kind: &str,
    required: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, InstructionError> {
    let Value::Object(object) = value else {
        return Err(InstructionError::Corruption {
            reason: format!("{kind} data is not an object"),
        });
    };
    let missing = required
        .iter()
        .filter(|field| !object.contains_key(**field))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(object);
    }
    Err(InstructionError::Corruption {
        reason: format!("{kind} has missing fields {missing:?}"),
    })
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    kind: &str,
) -> Result<&'a str, InstructionError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| InstructionError::Corruption {
            reason: format!("{kind} {field} must be a string"),
        })
}

fn parse_id<T>(value: &str, field: &str) -> Result<T, InstructionError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .parse()
        .map_err(|source| InstructionError::Corruption {
            reason: format!("{field} has an invalid identity: {source}"),
        })
}
