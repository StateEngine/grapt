//! Stored subagent-operation delivery decoding and atomic parent admission.

use super::*;

struct RawStoredOperationDelivery {
    operation: String,
    parent: String,
    input: String,
    claim_generation: i64,
    run_generation: Option<i64>,
    status: String,
    output: Option<String>,
    artifact_id: Option<String>,
    artifact_length: Option<i64>,
    artifact_digest: Option<String>,
    artifact_disposition: Option<String>,
    error: Option<String>,
    reason: Option<String>,
    evidence: Option<String>,
    route: String,
    target_parent: String,
    delivery: String,
    delivery_settlement: Option<String>,
}

struct StoredOperationDelivery {
    session: SessionId,
    input: MessageId,
    operation: OperationId,
    generation: RuntimeGeneration,
    settlement: OperationDeliverySettlement,
    artifact: Option<ArtifactRef>,
    projection: String,
}

fn corrupt_delivery_authority(
    subagent: &SubagentOperationId,
    reason: impl Into<String>,
) -> InboxError {
    InboxError::CorruptOperationDeliveryAuthority {
        subagent: subagent.clone(),
        reason: reason.into(),
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn load_stored_operation_delivery(
    tx: &GovernedTransaction<'_>,
    subagent: &SubagentOperationId,
    claim: &DeliveryClaimId,
) -> Result<StoredOperationDelivery, InboxError> {
    let raw = tx
        .query_row(
            "SELECT family_operation_id, parent_session_id, target_input,
                    claim_generation, run_generation, status, output,
                    artifact_id, artifact_length, artifact_digest, artifact_disposition,
                    error, reason, evidence, claim_route, target_parent,
                    delivery, delivery_settlement
             FROM subagent_operation
             WHERE operation_id = ?1 AND claim_id = ?2",
            rusqlite::params![subagent.as_str(), claim.as_str()],
            |row| {
                Ok(RawStoredOperationDelivery {
                    operation: row.get(0)?,
                    parent: row.get(1)?,
                    input: row.get(2)?,
                    claim_generation: row.get(3)?,
                    run_generation: row.get(4)?,
                    status: row.get(5)?,
                    output: row.get(6)?,
                    artifact_id: row.get(7)?,
                    artifact_length: row.get(8)?,
                    artifact_digest: row.get(9)?,
                    artifact_disposition: row.get(10)?,
                    error: row.get(11)?,
                    reason: row.get(12)?,
                    evidence: row.get(13)?,
                    route: row.get(14)?,
                    target_parent: row.get(15)?,
                    delivery: row.get(16)?,
                    delivery_settlement: row.get(17)?,
                })
            },
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => InboxError::MissingOperationDeliveryAuthority {
                subagent: subagent.clone(),
                claim: claim.clone(),
            },
            other => InboxError::Sqlite(other),
        })?;

    if raw.delivery != "delivering" {
        return Err(corrupt_delivery_authority(
            subagent,
            format!("delivery {:?} is not delivering", raw.delivery),
        ));
    }
    if raw.delivery_settlement.is_some() {
        return Err(corrupt_delivery_authority(
            subagent,
            "delivering row already carries a settlement",
        ));
    }
    if raw.route != "synthetic" {
        return Err(corrupt_delivery_authority(
            subagent,
            format!("claim route {:?} is not operation delivery", raw.route),
        ));
    }
    if raw.parent != raw.target_parent {
        return Err(corrupt_delivery_authority(
            subagent,
            "claim target parent differs from immutable parent",
        ));
    }
    let run_generation = raw.run_generation.ok_or_else(|| {
        corrupt_delivery_authority(subagent, "terminal row has no run generation")
    })?;
    if raw.claim_generation != run_generation {
        return Err(corrupt_delivery_authority(
            subagent,
            "claim generation differs from terminal run generation",
        ));
    }
    let generation = u64::try_from(raw.claim_generation)
        .map(RuntimeGeneration::new)
        .map_err(|_| corrupt_delivery_authority(subagent, "claim generation is negative"))?;
    let session: SessionId = raw.parent.parse().map_err(|error: grapt_types::IdError| {
        corrupt_delivery_authority(subagent, format!("parent session id: {error}"))
    })?;
    let input: MessageId = raw.input.parse().map_err(|error: grapt_types::IdError| {
        corrupt_delivery_authority(subagent, format!("target input id: {error}"))
    })?;
    let operation = OperationId::new(raw.operation)
        .ok_or_else(|| corrupt_delivery_authority(subagent, "family operation id is empty"))?;
    let has_no_artifact_payload = raw.output.is_none()
        && raw.artifact_id.is_none()
        && raw.artifact_length.is_none()
        && raw.artifact_digest.is_none()
        && raw.artifact_disposition.is_none();

    let (settlement, artifact, projection) = match raw.status.as_str() {
        "completed"
            if raw.error.is_none()
                && raw.reason.is_none()
                && raw.evidence.is_none()
                && raw.output.is_some()
                && raw.artifact_id.is_some()
                && raw.artifact_length.is_some()
                && raw.artifact_digest.is_some()
                && raw.artifact_disposition.as_deref().is_some_and(|value| {
                    value == "complete"
                        || value
                            .strip_prefix("truncated:")
                            .and_then(|captured| captured.parse::<u64>().ok())
                            .is_some()
                }) =>
        {
            let artifact_id = ArtifactId::new(raw.artifact_id.ok_or_else(|| {
                corrupt_delivery_authority(subagent, "completed row has no artifact id")
            })?)
            .ok_or_else(|| corrupt_delivery_authority(subagent, "artifact id is empty"))?;
            let artifact_length_raw = raw.artifact_length.ok_or_else(|| {
                corrupt_delivery_authority(subagent, "completed row has no artifact length")
            })?;
            let artifact_length = u64::try_from(artifact_length_raw)
                .map_err(|_| corrupt_delivery_authority(subagent, "artifact length is negative"))?;
            let artifact_digest = ArtifactDigest::new(raw.artifact_digest.ok_or_else(|| {
                corrupt_delivery_authority(subagent, "completed row has no artifact digest")
            })?)
            .ok_or_else(|| corrupt_delivery_authority(subagent, "artifact digest is empty"))?;
            (
                OperationDeliverySettlement::Completed,
                Some(ArtifactRef::new(
                    artifact_id,
                    artifact_length,
                    artifact_digest,
                )),
                raw.output.ok_or_else(|| {
                    corrupt_delivery_authority(subagent, "completed row has no projection")
                })?,
            )
        }
        "failed"
            if raw.error.as_deref().is_some_and(|value| !value.is_empty())
                && raw.reason.is_none()
                && raw.evidence.is_none()
                && has_no_artifact_payload =>
        {
            (
                OperationDeliverySettlement::Failed,
                None,
                raw.error.ok_or_else(|| {
                    corrupt_delivery_authority(subagent, "failed row has no error")
                })?,
            )
        }
        "cancelled"
            if matches!(raw.reason.as_deref(), Some("process-loss" | "interrupted"))
                && raw.error.is_none()
                && raw.evidence.is_none()
                && has_no_artifact_payload =>
        {
            (
                OperationDeliverySettlement::Failed,
                None,
                raw.reason.ok_or_else(|| {
                    corrupt_delivery_authority(subagent, "cancelled row has no reason")
                })?,
            )
        }
        "ambiguous"
            if raw
                .evidence
                .as_deref()
                .is_some_and(|value| !value.is_empty())
                && raw.error.is_none()
                && raw.reason.is_none()
                && has_no_artifact_payload =>
        {
            (
                OperationDeliverySettlement::Quarantined,
                None,
                raw.evidence.ok_or_else(|| {
                    corrupt_delivery_authority(subagent, "ambiguous row has no evidence")
                })?,
            )
        }
        _ => {
            return Err(corrupt_delivery_authority(
                subagent,
                format!("status {:?} or its payload is not deliverable", raw.status),
            ));
        }
    };

    Ok(StoredOperationDelivery {
        session,
        input,
        operation,
        generation,
        settlement,
        artifact,
        projection,
    })
}

pub(super) const CREATE_OPERATION_DELIVERY_PENDING: &str =
    "CREATE TABLE IF NOT EXISTS session_operation_delivery_pending (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        operation_id TEXT NOT NULL,
        subagent_operation_id TEXT NOT NULL,
        claim_id TEXT NOT NULL,
        generation INTEGER NOT NULL CHECK (generation >= 0),
        settlement TEXT NOT NULL CHECK (settlement IN ('completed', 'failed', 'quarantined')),
        artifact_id TEXT,
        artifact_length INTEGER CHECK (artifact_length IS NULL OR artifact_length >= 0),
        artifact_digest TEXT,
        projection TEXT NOT NULL,
        projection_bytes INTEGER NOT NULL CHECK (projection_bytes >= 0),
        admitted_seq INTEGER NOT NULL,
        UNIQUE(operation_id, claim_id),
        UNIQUE(subagent_operation_id, claim_id),
        CHECK ((settlement = 'completed') = (artifact_id IS NOT NULL)),
        CHECK ((artifact_id IS NULL) = (artifact_length IS NULL)),
        CHECK ((artifact_id IS NULL) = (artifact_digest IS NULL))
    );";

impl Inbox {
    /// Atomically admit one stored subagent operation delivery. Every event
    /// coordinate is loaded from the exact claimed durable operation row under
    /// this same transaction; callers cannot supply a parent, generation,
    /// terminal class, artifact, or projection.
    pub fn admit_subagent_operation_delivery_within(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        event_id: &EventId,
        authority: (&SubagentOperationId, &DeliveryClaimId),
        limits: (u64, u64),
        created_ms: i64,
    ) -> Result<OperationDeliverySettlement, InboxError> {
        let (subagent, claim) = authority;
        let (max_pending, max_projection_bytes) = limits;
        let stored = load_stored_operation_delivery(tx, subagent, claim)?;
        let projection_bytes =
            u64::try_from(stored.projection.len()).map_err(|_| InboxError::DeliveryCapacity {
                session: stored.session.clone(),
                pending: u64::MAX,
                max_pending,
                bytes: u64::MAX,
                max_bytes: max_projection_bytes,
            })?;
        let (pending_raw, bytes_raw): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(projection_bytes), 0)
             FROM session_operation_delivery_pending WHERE session_id = ?1",
            [stored.session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let pending = u64::try_from(pending_raw).map_err(|_| InboxError::DeliveryCapacity {
            session: stored.session.clone(),
            pending: u64::MAX,
            max_pending,
            bytes: u64::MAX,
            max_bytes: max_projection_bytes,
        })?;
        let bytes = u64::try_from(bytes_raw).map_err(|_| InboxError::DeliveryCapacity {
            session: stored.session.clone(),
            pending,
            max_pending,
            bytes: u64::MAX,
            max_bytes: max_projection_bytes,
        })?;
        let next_pending = pending
            .checked_add(1)
            .ok_or_else(|| InboxError::DeliveryCapacity {
                session: stored.session.clone(),
                pending: u64::MAX,
                max_pending,
                bytes,
                max_bytes: max_projection_bytes,
            })?;
        let next_bytes =
            bytes
                .checked_add(projection_bytes)
                .ok_or_else(|| InboxError::DeliveryCapacity {
                    session: stored.session.clone(),
                    pending: next_pending,
                    max_pending,
                    bytes: u64::MAX,
                    max_bytes: max_projection_bytes,
                })?;
        if next_pending > max_pending || next_bytes > max_projection_bytes {
            return Err(InboxError::DeliveryCapacity {
                session: stored.session.clone(),
                pending: next_pending,
                max_pending,
                bytes: next_bytes,
                max_bytes: max_projection_bytes,
            });
        }
        let mut input_value = json!({
            "type": "operation-delivery",
            "operationID": stored.operation.as_str(),
            "subagentOperationID": subagent.as_str(),
            "claimID": claim.as_str(),
            "generation": stored.generation.value(),
            "settlement": stored.settlement.as_str(),
            "projection": stored.projection,
        });
        if let Some(artifact) = &stored.artifact {
            input_value["artifact"] = json!({
                "id": artifact.id().as_str(),
                "length": artifact.length(),
                "digest": artifact.digest().as_str(),
            });
        }
        let data = json!({
            "sessionID": stored.session.as_str(),
            "inputID": stored.input.as_str(),
            "input": input_value,
        });
        let version = EventVersion::new(1).ok_or(InboxError::Version)?;
        let aggregate = AggregateId::Session(stored.session.clone());
        EventStore::append_within(
            tx,
            AppendInput {
                id: event_id,
                aggregate: aggregate.clone(),
                writer: held.token(&aggregate)?,
                kind_name: "session.input.admitted",
                version,
                created_ms,
                data: &data,
            },
            &mut [&mut InboxProjector],
        )?;
        Ok(stored.settlement)
    }
}
