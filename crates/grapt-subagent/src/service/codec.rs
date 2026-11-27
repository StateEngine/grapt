//! Strict persisted-operation decoding and storage-edge conversions.

use super::*;

impl SubagentService {
    /// Bound persisted bytes before the shared strict decoder materializes them.
    pub(super) fn decode_bounded_row(
        conn: &rusqlite::Connection,
        operation: &SubagentOperationId,
    ) -> Result<DecodedRow, SubagentError> {
        let within_bound: bool = conn.query_row(
            "SELECT length(CAST(child_session_id AS BLOB)) <= 512 AND
                 length(CAST(family_operation_id AS BLOB)) +
                 length(CAST(child_session_id AS BLOB)) + length(CAST(agent AS BLOB)) +
                 length(CAST(parent_session_id AS BLOB)) + length(CAST(description AS BLOB)) +
                 length(CAST(mode AS BLOB)) + length(CAST(status AS BLOB)) +
                 COALESCE(length(CAST(model_provider AS BLOB)), 0) +
                 COALESCE(length(CAST(model_id AS BLOB)), 0) +
                 COALESCE(length(CAST(model_variant AS BLOB)), 0) +
                 COALESCE(length(CAST(tool_call_id AS BLOB)), 0) +
                 length(CAST(effective_graph_id AS BLOB)) +
                 COALESCE(length(CAST(output AS BLOB)), 0) +
                 COALESCE(length(CAST(artifact_id AS BLOB)), 0) +
                 COALESCE(length(CAST(artifact_digest AS BLOB)), 0) +
                 COALESCE(length(CAST(artifact_disposition AS BLOB)), 0) +
                 COALESCE(length(CAST(error AS BLOB)), 0) +
                 COALESCE(length(CAST(reason AS BLOB)), 0) +
                 COALESCE(length(CAST(evidence AS BLOB)), 0) +
                 length(CAST(delivery AS BLOB)) +
                 COALESCE(length(CAST(delivery_settlement AS BLOB)), 0) +
                 COALESCE(length(CAST(claim_id AS BLOB)), 0) +
                 COALESCE(length(CAST(claim_route AS BLOB)), 0) +
                 COALESCE(length(CAST(target_parent AS BLOB)), 0) +
                 COALESCE(length(CAST(target_input AS BLOB)), 0) <= 1048576
                 FROM subagent_operation WHERE operation_id = ?1",
            [operation.as_str()],
            |row| row.get(0),
        )?;
        if !within_bound {
            return Err(SubagentError::Corruption {
                operation: operation.clone(),
                reason: "listed row exceeds the 1 MiB decode or 512-byte child address bound"
                    .into(),
            });
        }
        Self::decode_row(conn, operation)?.ok_or_else(|| SubagentError::Corruption {
            operation: operation.clone(),
            reason: "selected operation disappeared".into(),
        })
    }

    /// Strictly decode a full operation row. EVERY persisted identity — child,
    /// agent, immutable parent route, and (when claimed) claim id / target
    /// parent / target input — is decoded to its validated newtype here, so a
    /// malformed persisted value is `Corruption` and callers never act on a raw
    /// string. Accepts any `&Connection` (a `Transaction` derefs to one).
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(super) fn decode_row(
        conn: &rusqlite::Connection,
        operation: &SubagentOperationId,
    ) -> Result<Option<DecodedRow>, SubagentError> {
        let corrupt = |reason: String| SubagentError::Corruption {
            operation: operation.clone(),
            reason,
        };
        let id_corrupt =
            |field: &'static str| move |e: grapt_types::IdError| corrupt(format!("{field}: {e}"));
        let raw = conn
            .query_row(
                "SELECT family_operation_id, child_session_id, agent, parent_session_id,
                        description, mode, status, generation, run_generation, output,
                        artifact_id, artifact_length, artifact_digest, artifact_disposition,
                        error, reason, evidence, delivery, delivery_settlement,
                        claim_id, claim_generation,
                        claim_route, target_parent, target_input, time_acknowledged, time_cleaned
                 FROM subagent_operation WHERE operation_id = ?1",
                [operation.as_str()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, Option<i64>>(7)?,
                        r.get::<_, Option<i64>>(8)?,
                        r.get::<_, Option<String>>(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, Option<i64>>(11)?,
                        r.get::<_, Option<String>>(12)?,
                        r.get::<_, Option<String>>(13)?,
                        r.get::<_, Option<String>>(14)?,
                        r.get::<_, Option<String>>(15)?,
                        r.get::<_, Option<String>>(16)?,
                        r.get::<_, String>(17)?,
                        r.get::<_, Option<String>>(18)?,
                        r.get::<_, Option<String>>(19)?,
                        r.get::<_, Option<i64>>(20)?,
                        r.get::<_, Option<String>>(21)?,
                        r.get::<_, Option<String>>(22)?,
                        r.get::<_, Option<String>>(23)?,
                        r.get::<_, Option<i64>>(24)?,
                        r.get::<_, Option<i64>>(25)?,
                    ))
                },
            )
            .map(Some)
            .or_else(none_if_absent)?;
        let Some((
            family_operation,
            child,
            agent,
            parent,
            description,
            mode,
            status_raw,
            generation,
            run_generation,
            output,
            artifact_id,
            artifact_length,
            artifact_digest,
            artifact_disposition,
            error,
            reason,
            evidence,
            delivery_raw,
            delivery_settlement_raw,
            claim_id,
            claim_generation,
            claim_route,
            target_parent,
            target_input,
            time_acknowledged,
            time_cleaned,
        )) = raw
        else {
            return Ok(None);
        };

        let _family_operation = OperationId::new(family_operation)
            .ok_or_else(|| corrupt("family operation id is empty".into()))?;
        let child: SessionId = child.parse().map_err(id_corrupt("child session id"))?;
        let _agent = AgentId::new(agent).ok_or_else(|| corrupt("agent id is empty".into()))?;
        if description.is_empty() {
            return Err(corrupt("description is empty".into()));
        }
        let parent: SessionId = parent.parse().map_err(id_corrupt("parent session id"))?;
        let mode = SubagentMode::from_label(&mode)
            .ok_or_else(|| corrupt(format!("mode {mode:?} invalid")))?;
        // The remaining admission identities (model, tool call, effective graph)
        // are validated to their newtypes too — fail closed before any caller
        // transitions or derives a delivery from this row.
        Self::validate_admission_identities(conn, operation)?;
        let run_generation = run_generation
            .map(|raw| {
                u64::try_from(raw)
                    .map(RuntimeGeneration::new)
                    .map_err(|_| corrupt(format!("run generation {raw} is negative")))
            })
            .transpose()?;

        let (status, terminal) = match status_raw.as_str() {
            "admitted" => (OperationStatus::Admitted, None),
            "running" => {
                let raw =
                    generation.ok_or_else(|| corrupt("running without a generation".into()))?;
                let generation = u64::try_from(raw)
                    .map_err(|_| corrupt(format!("generation {raw} is negative")))?;
                (
                    OperationStatus::Running(RuntimeGeneration::new(generation)),
                    None,
                )
            }
            "completed" => {
                let artifact_id = ArtifactId::new(
                    artifact_id.ok_or_else(|| corrupt("completed without artifact id".into()))?,
                )
                .ok_or_else(|| corrupt("completed with empty artifact id".into()))?;
                let artifact_length_raw = artifact_length
                    .ok_or_else(|| corrupt("completed without artifact length".into()))?;
                let artifact_length = u64::try_from(artifact_length_raw).map_err(|_| {
                    corrupt(format!("artifact length {artifact_length_raw} is negative"))
                })?;
                let artifact_digest = ArtifactDigest::new(
                    artifact_digest
                        .ok_or_else(|| corrupt("completed without artifact digest".into()))?,
                )
                .ok_or_else(|| corrupt("completed with empty artifact digest".into()))?;
                let disposition = parse_disposition(
                    &artifact_disposition
                        .ok_or_else(|| corrupt("completed without artifact disposition".into()))?,
                )
                .ok_or_else(|| corrupt("completed with invalid artifact disposition".into()))?;
                let t = SubagentTerminal::Completed {
                    output: ArtifactBackedOutput::from_durable(
                        ArtifactRef::new(artifact_id, artifact_length, artifact_digest),
                        disposition,
                        output.ok_or_else(|| corrupt("completed without projection".into()))?,
                    ),
                };
                (OperationStatus::Terminal(t.clone()), Some(t))
            }
            "failed" => {
                let t = SubagentTerminal::Failed {
                    error: error.ok_or_else(|| corrupt("failed without error".into()))?,
                };
                (OperationStatus::Terminal(t.clone()), Some(t))
            }
            "cancelled" => {
                let reason = reason.ok_or_else(|| corrupt("cancelled without a reason".into()))?;
                let t = SubagentTerminal::Cancelled(
                    SubagentCancelReason::from_label(&reason)
                        .ok_or_else(|| corrupt(format!("cancel reason {reason:?} invalid")))?,
                );
                (OperationStatus::Terminal(t.clone()), Some(t))
            }
            "ambiguous" => (
                OperationStatus::ExternallyAmbiguous(
                    AmbiguityEvidence::new(
                        evidence.ok_or_else(|| corrupt("ambiguous without evidence".into()))?,
                    )
                    .ok_or_else(|| corrupt("ambiguous with blank evidence".into()))?,
                ),
                None,
            ),
            other => return Err(corrupt(format!("status {other:?} invalid"))),
        };

        let delivery = match delivery_raw.as_str() {
            "unclaimed" => {
                if claim_id.is_some()
                    || claim_generation.is_some()
                    || delivery_settlement_raw.is_some()
                {
                    return Err(corrupt("unclaimed row carries a claim".into()));
                }
                DecodedDelivery::Unclaimed
            }
            "delivering" | "delivered" => {
                let id = DeliveryClaimId::new(
                    claim_id.ok_or_else(|| corrupt("claimed row has no claim id".into()))?,
                )
                .ok_or_else(|| corrupt("empty claim id".into()))?;
                let route = route_of(
                    operation,
                    &claim_route.ok_or_else(|| corrupt("claimed row has no route".into()))?,
                )?;
                let raw_generation = claim_generation
                    .ok_or_else(|| corrupt("claimed row has no generation".into()))?;
                let generation =
                    RuntimeGeneration::new(u64::try_from(raw_generation).map_err(|_| {
                        corrupt(format!("claim generation {raw_generation} is negative"))
                    })?);
                let claim_parent: SessionId = target_parent
                    .ok_or_else(|| corrupt("claimed row has no target parent".into()))?
                    .parse()
                    .map_err(id_corrupt("target parent"))?;
                let claim_input: MessageId = target_input
                    .ok_or_else(|| corrupt("claimed row has no target input".into()))?
                    .parse()
                    .map_err(id_corrupt("target input"))?;
                // Only a domain terminal or explicit external ambiguity may
                // carry a claim. The latter is delivered as quarantine debt.
                if terminal.is_none() && !matches!(status, OperationStatus::ExternallyAmbiguous(_))
                {
                    return Err(corrupt(
                        "a claimed operation is neither terminal nor ambiguous".into(),
                    ));
                }
                if claim_parent != parent {
                    return Err(corrupt(
                        "claim target parent is not the operation's immutable parent route".into(),
                    ));
                }
                if time_cleaned.is_some() && time_acknowledged.is_none() {
                    return Err(corrupt("cleaned delivery is not acknowledged".into()));
                }
                if time_acknowledged.is_some() && delivery_raw != "delivered" {
                    return Err(corrupt("acknowledged delivery is not delivered".into()));
                }
                let settlement = match (delivery_raw.as_str(), delivery_settlement_raw.as_deref()) {
                    ("delivering", None) => None,
                    ("delivering", Some(_)) => {
                        return Err(corrupt("delivering row already carries settlement".into()));
                    }
                    ("delivered", Some("completed"))
                        if matches!(
                            status,
                            OperationStatus::Terminal(SubagentTerminal::Completed { .. })
                        ) =>
                    {
                        Some(DeliverySettlement::Completed)
                    }
                    ("delivered", Some("failed"))
                        if matches!(
                            status,
                            OperationStatus::Terminal(
                                SubagentTerminal::Failed { .. } | SubagentTerminal::Cancelled(_)
                            )
                        ) =>
                    {
                        Some(DeliverySettlement::Failed)
                    }
                    ("delivered", Some("quarantined"))
                        if matches!(status, OperationStatus::ExternallyAmbiguous(_)) =>
                    {
                        Some(DeliverySettlement::Quarantined)
                    }
                    ("delivered", Some(other)) => {
                        return Err(corrupt(format!(
                            "delivery settlement {other:?} contradicts operation status"
                        )));
                    }
                    ("delivered", None) => {
                        return Err(corrupt("delivered row has no settlement".into()));
                    }
                    _ => return Err(corrupt("invalid delivery settlement state".into())),
                };
                DecodedDelivery::Claimed {
                    id,
                    route,
                    parent: claim_parent,
                    input: claim_input,
                    generation,
                    delivered: delivery_raw == "delivered",
                    settlement,
                    acknowledged: time_acknowledged.is_some(),
                    cleaned: time_cleaned.is_some(),
                }
            }
            other => return Err(corrupt(format!("delivery {other:?} invalid"))),
        };

        Ok(Some(DecodedRow {
            subagent: operation.clone(),
            child,
            parent,
            mode,
            run_generation,
            status,
            delivery,
        }))
    }

    pub(super) fn exactly_one(
        operation: &SubagentOperationId,
        affected: usize,
    ) -> Result<(), SubagentError> {
        if affected == 1 {
            Ok(())
        } else {
            Err(SubagentError::Corruption {
                operation: operation.clone(),
                reason: format!("transition affected {affected} rows (expected exactly 1)"),
            })
        }
    }
}

/// Checked storage-edge conversion of a runtime generation to the `i64` SQLite
/// column — a value beyond `i64::MAX` is a typed refusal, never a wrapping cast.
pub(super) fn generation_to_i64(generation: RuntimeGeneration) -> Result<i64, SubagentError> {
    i64::try_from(generation.value()).map_err(|_| SubagentError::GenerationOutOfRange {
        value: generation.value(),
    })
}

/// The raw status label of a decoded status, for CAS from-state comparison.
pub(super) fn status_label(status: &OperationStatus) -> &'static str {
    match status {
        OperationStatus::Admitted => "admitted",
        OperationStatus::Running(_) => "running",
        OperationStatus::Terminal(t) => t.status_str(),
        OperationStatus::ExternallyAmbiguous(_) => "ambiguous",
    }
}

pub(super) struct TerminalColumns<'a> {
    pub(super) output: Option<&'a str>,
    pub(super) error: Option<&'a str>,
    pub(super) reason: Option<&'static str>,
    pub(super) artifact_id: Option<&'a str>,
    pub(super) artifact_length: Option<i64>,
    pub(super) artifact_digest: Option<&'a str>,
    pub(super) artifact_disposition: Option<String>,
}

impl<'a> TerminalColumns<'a> {
    pub(super) fn from_terminal(terminal: &'a SubagentTerminal) -> Result<Self, SubagentError> {
        match terminal {
            SubagentTerminal::Completed { output } => {
                let length = i64::try_from(output.artifact().length()).map_err(|_| {
                    SubagentError::ArtifactLengthOutOfRange {
                        value: output.artifact().length(),
                    }
                })?;
                Ok(Self {
                    output: Some(output.projection()),
                    error: None,
                    reason: None,
                    artifact_id: Some(output.artifact().id().as_str()),
                    artifact_length: Some(length),
                    artifact_digest: Some(output.artifact().digest().as_str()),
                    artifact_disposition: Some(render_disposition(output.disposition())),
                })
            }
            SubagentTerminal::Failed { error } => Ok(Self {
                output: None,
                error: Some(error.as_str()),
                reason: None,
                artifact_id: None,
                artifact_length: None,
                artifact_digest: None,
                artifact_disposition: None,
            }),
            SubagentTerminal::Cancelled(reason) => Ok(Self {
                output: None,
                error: None,
                reason: Some(reason.as_str()),
                artifact_id: None,
                artifact_length: None,
                artifact_digest: None,
                artifact_disposition: None,
            }),
        }
    }
}

fn render_disposition(disposition: CaptureDisposition) -> String {
    match disposition {
        CaptureDisposition::Complete => "complete".to_owned(),
        CaptureDisposition::TruncatedAtBoundary { captured } => format!("truncated:{captured}"),
    }
}

fn parse_disposition(raw: &str) -> Option<CaptureDisposition> {
    if raw == "complete" {
        return Some(CaptureDisposition::Complete);
    }
    raw.strip_prefix("truncated:")
        .and_then(|value| value.parse::<u64>().ok())
        .map(|captured| CaptureDisposition::TruncatedAtBoundary { captured })
}

fn route_of(operation: &SubagentOperationId, raw: &str) -> Result<ClaimRoute, SubagentError> {
    ClaimRoute::from_label(raw).ok_or_else(|| SubagentError::Corruption {
        operation: operation.clone(),
        reason: format!("claim route {raw:?} outside the vocabulary"),
    })
}

fn none_if_absent<T>(e: rusqlite::Error) -> Result<Option<T>, rusqlite::Error> {
    if e == rusqlite::Error::QueryReturnedNoRows {
        Ok(None)
    } else {
        Err(e)
    }
}
