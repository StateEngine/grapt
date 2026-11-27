//! Immutable admission readback, independent of pending or promoted projections.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputAcceptance {
    pub session: SessionId,
    pub input: MessageId,
    pub seq: u64,
    pub created_ms: i64,
    pub kind: PromotedInputKind,
    pub text: String,
    pub metadata: Option<Value>,
    pub delivery: DeliveryMode,
}

impl Inbox {
    /// Read durable acceptance without taking a write lock or opening a transaction.
    pub fn input_acceptance(
        &self,
        session: &SessionId,
        input: &MessageId,
    ) -> Result<Option<InputAcceptance>, InboxError> {
        Ok(read_admission(self.store.connection(), session, input)?.map(|(receipt, _)| receipt))
    }
}

/// Retain canonical provenance for the external-notice retry check as well.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn read_admission(
    connection: &rusqlite::Connection,
    session: &SessionId,
    input: &MessageId,
) -> Result<Option<(InputAcceptance, Value)>, InboxError> {
    let corrupt = |reason: &str| InboxError::Corruption {
        input: input.clone(),
        reason: reason.into(),
    };
    // SQL bounds payload allocation and the duplicate witness. Malformed JSON
    // and malformed top-level identities cannot turn an admission into an
    // unknown input. Check key multiplicity before filtering by inputID: SQLite
    // reads the first duplicate member while serde_json reads the last.
    let mut statement = connection.prepare(
        "WITH admissions AS NOT MATERIALIZED (
             SELECT seq, created, aggregate_family, aggregate_id, data,
                    CASE WHEN typeof(data)='text'
                              AND length(CAST(data AS BLOB))<=4194304
                         THEN CASE WHEN json_valid(data)
                              THEN json_type(data, '$.inputID')='text'
                                   AND json_type(data, '$.sessionID')='text'
                                   AND length(CAST(json_extract(data, '$.inputID') AS BLOB))>0
                                   AND length(CAST(json_extract(data, '$.sessionID') AS BLOB))>0
                                   AND (SELECT count(*) FROM json_each(data)
                                        WHERE key='inputID')=1
                                   AND (SELECT count(*) FROM json_each(data)
                                        WHERE key='sessionID')=1
                              END END AS valid_identity
             FROM event WHERE type='session.input.admitted.1'
         )
         SELECT seq, created,
                CASE WHEN aggregate_family='session' AND valid_identity
                     THEN json_extract(data, '$.sessionID')=aggregate_id END,
                CASE WHEN length(CAST(data AS BLOB))<=4194304 THEN data END,
                CASE WHEN length(CAST(aggregate_id AS BLOB))<=512 THEN aggregate_id END
         FROM admissions
         WHERE CASE WHEN valid_identity THEN json_extract(data, '$.inputID')=?1
                    ELSE 1 END
         LIMIT 2",
    )?;
    let mut rows = statement.query([input.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let sequence: i64 = row.get(0)?;
    let created_ms: i64 = row.get(1)?;
    let owner_matches: Option<bool> = row.get(2)?;
    let raw: Option<String> = row.get(3)?;
    let aggregate: Option<String> = row.get(4)?;
    if rows.next()?.is_some() {
        return Err(corrupt("more than one admitted event for one input"));
    }
    let raw =
        raw.ok_or_else(|| corrupt("admitted event payload exceeds 4 MiB or is unreadable"))?;
    let data: Value = serde_json::from_str(&raw)
        .map_err(|_| corrupt("admitted event payload is not valid JSON"))?;
    if owner_matches != Some(true) {
        return Err(corrupt(
            "admitted event Session disagrees with its aggregate",
        ));
    }
    let decoded = InboxEvent::decode("session.input.admitted", 1, &data)
        .map_err(|error| corrupt(&error.to_string()))?;
    let (owner, identity, delivery, kind) = match decoded {
        Some(InboxEvent::Admitted {
            session,
            input,
            delivery,
        }) => (session, input, delivery, PromotedInputKind::User),
        Some(InboxEvent::SyntheticAdmitted {
            session,
            input,
            delivery,
        }) => (session, input, delivery, PromotedInputKind::Synthetic),
        _ => {
            return Err(corrupt(
                "admitted event is not active user or synthetic input",
            ))
        }
    };
    if identity != *input {
        return Err(corrupt(
            "admitted event identity disagrees with the requested input",
        ));
    }
    if owner != *session {
        return Err(InboxError::Conflict {
            input: input.clone(),
            differs: "session",
        });
    }
    // Corroborate serde's last sessionID against SQLite's first sessionID.
    if aggregate.as_deref() != Some(owner.as_str()) {
        return Err(corrupt(
            "decoded Session disagrees with the admitted aggregate",
        ));
    }
    let seq =
        u64::try_from(sequence).map_err(|_| corrupt("admitted event sequence is negative"))?;
    let text = data
        .pointer("/input/data/text")
        .and_then(Value::as_str)
        .ok_or_else(|| corrupt("admitted event has no input.data.text"))?
        .to_owned();
    let metadata = data.pointer("/input/data/metadata").cloned();
    Ok(Some((
        InputAcceptance {
            session: owner,
            input: identity,
            seq,
            created_ms,
            kind,
            text,
            metadata,
            delivery,
        },
        data,
    )))
}
