//! User-requested compaction: `session.compaction.admitted` projects one
//! pending `compaction` row per Session, and the request's manual terminal
//! settles it (the oracle's `SessionPending.admitCompaction` and
//! `settleCompaction`). It never holds input: it runs inside the next promoted
//! input's first Step.
use super::*;

#[derive(Debug)]
pub struct CompactionAdmitRequest<'a> {
    pub event_id: &'a EventId,
    pub session: &'a SessionId,
    pub input: &'a MessageId,
    pub created_ms: i64,
    pub instructions: Option<&'a str>,
}

/// Why a user-requested compaction ended without a descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionFailure<'a> {
    /// Interrupted before it published (oracle error type `aborted`).
    Aborted,
    /// Failed or made no progress (oracle error type `compaction.failed`).
    Failed(&'a str),
}

#[derive(Debug)]
pub struct CompactionFailRequest<'a> {
    pub event_id: &'a EventId,
    pub session: &'a SessionId,
    pub failure: CompactionFailure<'a>,
    pub created_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCompaction {
    pub input: MessageId,
    pub seq: u64,
    /// The admission's own durable time (`SessionPending.timeCreated`).
    pub created_ms: i64,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionAdmission {
    Admitted(PendingCompaction),
    /// This request's own replay, or another request already pending. Nothing
    /// was appended.
    AlreadyPending(PendingCompaction),
}

impl CompactionAdmission {
    pub fn pending(&self) -> &PendingCompaction {
        match self {
            Self::Admitted(pending) | Self::AlreadyPending(pending) => pending,
        }
    }
}

impl Inbox {
    /// Admit a user-requested compaction: the decision reads, the append and
    /// its projection in one immediate transaction, as every admission here.
    pub fn admit_compaction(
        &mut self,
        request: CompactionAdmitRequest<'_>,
    ) -> Result<CompactionAdmission, InboxError> {
        let mut data = json!({
            "sessionID": request.session.as_str(),
            "inputID": request.input.as_str(),
        });
        if let Some(instructions) = request.instructions {
            data["instructions"] = json!(instructions);
        }
        crate::event::compaction_instructions(&data).map_err(|_| {
            InboxError::PendingBound(
                "compaction instructions must contain text and fit within 16 KiB",
            )
        })?;
        let held = self.store.held_writers();
        let aggregate = AggregateId::Session(request.session.clone());
        let tx = self.store.governed_transaction()?;
        if let Some(existing) = Self::find_existing(&tx, request.input)? {
            let differs = if existing.session != request.session.as_str() {
                "session"
            } else if existing.input_type != "compaction" {
                "type"
            } else {
                // This Session's own compaction id: its replay while pending,
                // or a request that already ran and never runs again.
                return match pending_in(&tx, request.session)? {
                    Some(pending) if &pending.input == request.input => {
                        same_instructions(&pending, &request)?;
                        tx.commit()?.discard();
                        Ok(CompactionAdmission::AlreadyPending(pending))
                    }
                    _ => Err(InboxError::CompactionSettled {
                        input: request.input.clone(),
                    }),
                };
            };
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs,
            });
        }
        if let Some(pending) = pending_in(&tx, request.session)? {
            same_instructions(&pending, &request)?;
            tx.commit()?.discard();
            return Ok(CompactionAdmission::AlreadyPending(pending));
        }
        let version = EventVersion::new(1).ok_or(InboxError::Version)?;
        let appended = EventStore::append_within(
            &tx,
            AppendInput {
                id: request.event_id,
                aggregate: aggregate.clone(),
                writer: held.token(&aggregate)?,
                kind_name: "session.compaction.admitted",
                version,
                created_ms: request.created_ms,
                data: &data,
            },
            &mut [&mut InboxProjector],
        )?;
        tx.commit()?.discard();
        let seq = match appended {
            Appended::Committed { seq } | Appended::Idempotent { seq } => seq,
        };
        Ok(CompactionAdmission::Admitted(PendingCompaction {
            input: request.input.clone(),
            seq,
            created_ms: request.created_ms,
            instructions: request.instructions.map(str::to_owned),
        }))
    }

    /// Settle the pending compaction as failed: the oracle's
    /// `session.compaction.failed` with reason `manual`, appended only while
    /// it is still pending (`SessionRunner.runPendingCompaction`). Returns the
    /// settled compaction, or `None` when nothing was pending.
    pub fn fail_compaction(
        &mut self,
        request: CompactionFailRequest<'_>,
    ) -> Result<Option<PendingCompaction>, InboxError> {
        let held = self.store.held_writers();
        let aggregate = AggregateId::Session(request.session.clone());
        let tx = self.store.governed_transaction()?;
        let Some(pending) = pending_in(&tx, request.session)? else {
            tx.commit()?.discard();
            return Ok(None);
        };
        let (kind, message) = match request.failure {
            CompactionFailure::Aborted => ("aborted", "Compaction cancelled".to_owned()),
            CompactionFailure::Failed(reason) => ("compaction.failed", reason.to_owned()),
        };
        let data = json!({
            "sessionID": request.session.as_str(),
            "reason": "manual",
            "error": {"type": kind, "message": message},
            "inputID": pending.input.as_str(),
        });
        let version = EventVersion::new(1).ok_or(InboxError::Version)?;
        EventStore::append_within(
            &tx,
            AppendInput {
                id: request.event_id,
                aggregate: aggregate.clone(),
                writer: held.token(&aggregate)?,
                kind_name: "session.compaction.failed",
                version,
                created_ms: request.created_ms,
                data: &data,
            },
            &mut [&mut InboxProjector],
        )?;
        tx.commit()?.discard();
        Ok(Some(pending))
    }

    /// The Session's pending user-requested compaction, if any.
    pub fn pending_compaction(
        &self,
        session: &SessionId,
    ) -> Result<Option<PendingCompaction>, InboxError> {
        pending_in(self.store.connection(), session)
    }
}

/// At most one row: the projector refuses a second admission, so two is
/// corruption rather than a choice.
pub(super) fn pending_in(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<PendingCompaction>, InboxError> {
    let mut query = connection.prepare(
        "SELECT p.id, p.admitted_seq, e.created,
                CASE WHEN length(CAST(e.data AS BLOB)) <= 131072 THEN e.data ELSE NULL END
         FROM session_pending p
         LEFT JOIN event e ON e.aggregate_family = 'session'
              AND e.aggregate_id = p.session_id AND e.seq = p.admitted_seq
              AND e.type = 'session.compaction.admitted.1'
         WHERE p.session_id = ?1 AND p.input_type = 'compaction'
         ORDER BY p.admitted_seq LIMIT 2",
    )?;
    let rows = query
        .query_map([session.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let Some((raw, seq, created, data)) = rows.first() else {
        return Ok(None);
    };
    let input: MessageId =
        raw.parse()
            .map_err(|e: grapt_types::IdError| InboxError::CorruptPendingId {
                raw: raw.clone(),
                reason: e.to_string(),
            })?;
    if rows.len() > 1 {
        return Err(InboxError::Corruption {
            input,
            reason: format!("session {session} has more than one pending compaction"),
        });
    }
    let seq = u64::try_from(*seq).map_err(|_| InboxError::Corruption {
        input: input.clone(),
        reason: "negative compaction admission sequence".into(),
    })?;
    let created_ms =
        created
            .filter(|created| *created >= 0)
            .ok_or_else(|| InboxError::Corruption {
                input: input.clone(),
                reason: "pending compaction has no valid admission event".into(),
            })?;
    let instructions = data
        .as_deref()
        .ok_or("pending compaction admission is absent or oversized")
        .and_then(|raw| {
            serde_json::from_str::<Value>(raw).map_err(|_| "invalid compaction admission JSON")
        })
        .and_then(|data| {
            crate::event::compaction_instructions(&data)
                .map(|text| text.map(str::to_owned))
                .map_err(|_| "invalid compaction instructions")
        })
        .map_err(|reason| InboxError::Corruption {
            input: input.clone(),
            reason: reason.into(),
        })?;
    Ok(Some(PendingCompaction {
        input,
        seq,
        created_ms,
        instructions,
    }))
}

fn same_instructions(
    pending: &PendingCompaction,
    request: &CompactionAdmitRequest<'_>,
) -> Result<(), InboxError> {
    if pending.instructions.as_deref() != request.instructions {
        return Err(InboxError::Conflict {
            input: request.input.clone(),
            differs: "instructions",
        });
    }
    Ok(())
}
