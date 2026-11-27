//! Fixed product text beside a failed physical attempt; never vendor text.
use super::*;
use grapt_types::PhysicalAttemptId;

pub(super) const PROVIDER_ERROR_DETAIL: &str = "provider-error-detail";

/// Only these product-authored outcomes can enter the notice log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderErrorOutcome {
    EnableHint,
    Saved,
    Unavailable,
}
impl ProviderErrorOutcome {
    pub const fn text(self) -> &'static str {
        match self {
            Self::EnableHint => {
                "set provider_error_text=true in service.conf to save the provider's error text"
            }
            Self::Saved => {
                "Provider error details saved to logs/provider-error.txt (last error only)."
            }
            Self::Unavailable => {
                "Provider error details could not be saved to logs/provider-error.txt."
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderErrorDetailNotice {
    pub position: CommittedPosition,
    pub execution: ExecutionId,
    pub attempt: PhysicalAttemptId,
    pub outcome: ProviderErrorOutcome,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    #[serde(rename = "attemptID")]
    attempt: String,
    kind: String,
    reason: String,
}
fn decode(
    session: &SessionId,
    position: CommittedPosition,
    data: Data,
) -> Result<ProviderErrorDetailNotice, String> {
    let outcome = [
        ProviderErrorOutcome::EnableHint,
        ProviderErrorOutcome::Saved,
        ProviderErrorOutcome::Unavailable,
    ]
    .into_iter()
    .find(|outcome| outcome.text() == data.reason)
    .ok_or("provider-error-detail notice contains non-product text")?;
    if data.session != session.as_str()
        || data.kind != PROVIDER_ERROR_DETAIL
        || data.attempt.len() > 256
    {
        return Err("provider-error-detail notice identity is invalid".into());
    }
    Ok(ProviderErrorDetailNotice {
        position,
        execution: ExecutionId::new(data.execution).ok_or("invalid notice execution")?,
        attempt: PhysicalAttemptId::new(data.attempt).ok_or("invalid notice attempt")?,
        outcome,
    })
}
/// The newest notice whose `field` names `value`, decoded to its execution.
fn prior_notice(
    tx: &grapt_store::GovernedTransaction<'_>,
    session: &SessionId,
    field: &str,
    value: &str,
) -> Result<Option<ProviderErrorDetailNotice>, String> {
    use rusqlite::OptionalExtension;
    let row: Option<(String, i64, i64, String)> = tx
        .query_row(
            "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 256 THEN id END, seq, created,
             CASE WHEN length(CAST(data AS BLOB)) <= ?4 THEN data END FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type = 'session.notice.recorded.1' AND json_extract(data, '$.kind') = ?2
             AND json_extract(data, ?3) = ?5 ORDER BY seq DESC LIMIT 1",
            rusqlite::params![
                session.as_str(),
                PROVIDER_ERROR_DETAIL,
                field,
                MAX_NOTICE_DATA_BYTES as i64,
                value
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    row.map(|(id, seq, created_ms, raw)| {
        if created_ms < 0 {
            return Err("notice time is negative".into());
        }
        let position = CommittedPosition {
            event: id
                .parse()
                .map_err(|error: grapt_types::IdError| error.to_string())?,
            seq: u64::try_from(seq).map_err(|error| error.to_string())?,
            created_ms,
        };
        decode(
            session,
            position,
            serde_json::from_str(&raw).map_err(|e| e.to_string())?,
        )
    })
    .transpose()
}

/// At most one notice per failed attempt, including after reopening — except the
/// enable hint, once per execution. No argument admits vendor text to this path.
pub fn record_provider_error_detail(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    attempt: &PhysicalAttemptId,
    outcome: ProviderErrorOutcome,
) -> Result<Option<ProviderErrorDetailNotice>, String> {
    if attempt.as_str().len() > 256 {
        return Err("notice attempt exceeds its byte bound".into());
    }
    let aggregate = AggregateId::Session(session.clone());
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|error| error.to_string())?;
    // Ownership first, for every kind: one attempt never changes execution.
    if let Some(prior) = prior_notice(&tx, session, "$.attemptID", attempt.as_str())? {
        if &prior.execution != execution {
            return Err("notice attempt belongs to another execution".into());
        }
        return Ok(None);
    }
    // The hint names a setting, so the execution claims it; other kinds are
    // already answered per attempt above.
    if outcome == ProviderErrorOutcome::EnableHint
        && prior_notice(&tx, session, "$.executionID", execution.as_str())?.is_some()
    {
        return Ok(None);
    }
    let data = serde_json::to_value(Data {
        session: session.as_str().into(),
        execution: execution.as_str().into(),
        attempt: attempt.as_str().into(),
        kind: PROVIDER_ERROR_DETAIL.into(),
        reason: outcome.text().into(),
    })
    .map_err(|error| error.to_string())?;
    let position = append_in(&tx, aggregate, writer, authorities, &data)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(Some(ProviderErrorDetailNotice {
        position,
        execution: execution.clone(),
        attempt: attempt.clone(),
        outcome,
    }))
}
pub fn provider_error_detail_notices(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<ProviderErrorDetailNotice>, String> {
    kind_rows::<Data>(connection, session, PROVIDER_ERROR_DETAIL)?
        .into_iter()
        .map(|(position, data)| decode(session, position, data))
        .collect()
}

/// Decode exactly the notice selected by a public history page.
pub(super) fn at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    sequence: u64,
) -> Result<Vec<ProviderErrorDetailNotice>, String> {
    kind_rows_at::<Data>(connection, session, PROVIDER_ERROR_DETAIL, Some(sequence))?
        .into_iter()
        .map(|(position, data)| decode(session, position, data))
        .collect()
}
