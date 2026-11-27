//! The existing input transaction owns native-mail consumption and admission.
use super::*;
use grapt_store::RemoteMailWakeClaim;
use rusqlite::OptionalExtension;

// Family is the leading key of both aggregate indexes. Keep recovery on the
// exact admission instead of scanning every session's admissions by type.
const PENDING_NATIVE_WAKE_ADMISSION: &str =
    "SELECT CASE WHEN length(CAST(data AS BLOB))<=4194304 THEN data END
    FROM event WHERE seq=?1 AND aggregate_family='session'
      AND aggregate_id=?2 AND type='session.input.admitted.1'";

#[cfg(test)]
#[path = "native_mail_query_tests.rs"]
mod query_tests;

impl Inbox {
    pub fn admit_native_mail_wake(
        &mut self,
        claim: &RemoteMailWakeClaim,
        request: SyntheticAdmitRequest<'_>,
    ) -> Result<AdmitOutcome, InboxError> {
        if claim.session() != request.session || request.metadata != Some(&claim.metadata()) {
            return Err(InboxError::PendingBound("native wake claim coordinates"));
        }
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        // A later mail claim must not reopen the ordinary pending fold and
        // promote work explicitly stopped by the user. Check inside the same
        // transaction as admission and consumption so interrupt cannot race it.
        // A genuine human resume authorizes retained input without deleting
        // its historical per-input hold. Fresh mail cannot grant that authority.
        refuse_held_work(&tx, request.session)?;
        let outcome = Self::admit_synthetic_owned_within(&tx, &held, &request, true)?;
        if matches!(outcome, AdmitOutcome::Reconciled { .. }) {
            return Err(InboxError::Conflict {
                input: request.input.clone(),
                differs: "native wake claim",
            });
        }
        // Even an exact input retry cannot steal another claim. Recovery uses
        // the already admitted pending input rather than consuming mail again.
        claim.consume_within(&tx)?;
        tx.commit()?.discard();
        Ok(outcome)
    }

    /// Recovery follows the canonical admission marker, never a client-supplied
    /// metadata key. Ordinary admit-only synthetic input stays admit-only.
    pub fn pending_native_mail_wake(
        &self,
        session: &SessionId,
    ) -> Result<Option<PendingInput>, InboxError> {
        for input in Self::pending_inputs_for_promotion_within(self.store.connection(), session)? {
            if !Self::native_mail_input_eligible_within(self.store.connection(), session, &input)? {
                continue;
            }
            if input.kind != PromotedInputKind::Synthetic {
                continue;
            }
            let sequence = i64::try_from(input.seq).map_err(|_| {
                InboxError::PendingBound("native wake sequence exceeds SQLite range")
            })?;
            let raw: Option<String> = self.store.connection().query_row(
                PENDING_NATIVE_WAKE_ADMISSION,
                rusqlite::params![sequence, session.as_str()],
                |row| row.get(0),
            )?;
            let invalid = || InboxError::Corruption {
                input: input.input.clone(),
                reason: "native wake admission provenance is unreadable".into(),
            };
            let value: Value =
                serde_json::from_str(&raw.ok_or_else(invalid)?).map_err(|_| invalid())?;
            if value.pointer("/input/nativeMailWake") == Some(&Value::Bool(true))
                && !native_wake_held(self.store.connection(), session, &input.input)?
            {
                return Ok(Some(input));
            }
        }
        Ok(None)
    }
}

pub(super) fn refuse_held_work(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<(), InboxError> {
    for input in Inbox::pending_inputs_for_promotion_within(connection, session)? {
        if native_wake_held(connection, session, &input.input)?
            && !super::native_mail_state::human_resumed(connection, session)?
        {
            return Err(InboxError::NativeWakeHeld);
        }
    }
    Ok(())
}

impl Inbox {
    /// An acknowledged explicit stop survives reconnect and process loss.
    /// The input stays in the ordinary pending fold for an explicit resume;
    /// only automatic native recovery excludes it. No projection is rewritten.
    pub fn hold_native_mail_wakes(
        &mut self,
        session: &SessionId,
        created_ms: i64,
    ) -> Result<usize, InboxError> {
        let held = self.store.held_writers();
        let tx = self.store.governed_transaction()?;
        super::native_mail_state::pause(&tx, &held, session, created_ms)?;
        let mut count = 0;
        for input in Self::pending_inputs_for_promotion_within(&tx, session)? {
            if input.kind != PromotedInputKind::Synthetic {
                continue;
            }
            let sequence = i64::try_from(input.seq).map_err(|_| {
                InboxError::PendingBound("native wake sequence exceeds SQLite range")
            })?;
            let raw: String = tx.query_row(
                "SELECT CASE WHEN length(CAST(data AS BLOB))<=4194304 THEN data END
                 FROM event WHERE seq=?1 AND aggregate_family='session'
                   AND aggregate_id=?2 AND type='session.input.admitted.1'",
                rusqlite::params![sequence, session.as_str()],
                |row| row.get(0),
            )?;
            let value: Value = serde_json::from_str(&raw)
                .map_err(|_| InboxError::PendingBound("native wake admission provenance"))?;
            if value.pointer("/input/nativeMailWake") != Some(&Value::Bool(true))
                || native_wake_held(&tx, session, &input.input)?
            {
                continue;
            }
            let aggregate = AggregateId::Session(session.clone());
            let event = native_hold_id(&input.input)?;
            let data = json!({"sessionID": session.as_str(), "inputID": input.input.as_str()});
            EventStore::append_within(
                &tx,
                AppendInput {
                    id: &event,
                    aggregate: aggregate.clone(),
                    writer: held.token(&aggregate)?,
                    kind_name: "session.input.native-wake-held",
                    version: EventVersion::new(1).ok_or(InboxError::Version)?,
                    created_ms,
                    data: &data,
                },
                &mut [&mut InboxProjector],
            )?;
            count += 1;
        }
        tx.commit()?.discard();
        Ok(count)
    }
}

fn native_hold_id(input: &MessageId) -> Result<EventId, InboxError> {
    if input.as_str().len() > 512 {
        return Err(InboxError::PendingBound("native wake hold identity"));
    }
    // Input IDs are globally unique. A reserved deterministic event ID gives
    // bounded exact-key recovery without a second table or history scan.
    format!("evt_native_wake_hold_{}", input.as_str())
        .parse()
        .map_err(|_| InboxError::PendingBound("native wake hold identity"))
}

pub(super) fn native_wake_held(
    connection: &rusqlite::Connection,
    session: &SessionId,
    input: &MessageId,
) -> Result<bool, InboxError> {
    let event = native_hold_id(input)?;
    let record: Option<(String, String, String, String)> = connection
        .query_row(
            "SELECT aggregate_family, aggregate_id, type,
                CASE WHEN length(CAST(data AS BLOB))<=4096 THEN data END
         FROM event WHERE id=?1",
            [event.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((family, owner, kind, raw)) = record else {
        return Ok(false);
    };
    let data: Value = serde_json::from_str(&raw)
        .map_err(|_| InboxError::PendingBound("native wake hold provenance"))?;
    if family != "session"
        || owner != session.as_str()
        || kind != "session.input.native-wake-held.1"
        || data != json!({"sessionID":session.as_str(),"inputID":input.as_str()})
    {
        return Err(InboxError::Corruption {
            input: input.clone(),
            reason: "native wake hold coordinates disagree".into(),
        });
    }
    Ok(true)
}
