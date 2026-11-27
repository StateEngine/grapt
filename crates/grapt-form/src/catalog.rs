//! Bounded read projection of unsettled forms from their durable owner.
//!
//! Suspension remains listable. The creation event supplies the original field
//! specification; a missing or conflicting event is corruption, never an empty
//! form. This borrows the existing connection and does not repair on read.

use grapt_store::EventStore;
use grapt_types::{FormId, FormSuspensionReason, SessionId};
use serde_json::Value;

pub const MAX_FORM_CATALOG_ROWS: usize = 128;
const MAX_FORM_EVENT_BYTES: usize = 256 * 1024;
const MAX_FORM_CATALOG_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct PendingForm {
    pub id: FormId,
    pub session: SessionId,
    pub info: Value,
    pub suspended: Option<FormSuspensionReason>,
}

#[derive(Debug, thiserror::Error)]
pub enum FormCatalogError {
    #[error("form catalog database: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("form catalog refuses incomplete evidence: {0}")]
    Evidence(String),
}

pub fn pending_forms(store: &EventStore) -> Result<Vec<PendingForm>, FormCatalogError> {
    pending_forms_for(store, None)
}

pub fn pending_forms_for(
    store: &EventStore,
    session: Option<&SessionId>,
) -> Result<Vec<PendingForm>, FormCatalogError> {
    let mut statement = store.connection().prepare(
        "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 512 THEN id END,
                CASE WHEN length(CAST(session_id AS BLOB)) <= 512 THEN session_id END,
                CASE WHEN length(CAST(title AS BLOB)) <= 262144 THEN title END,
                CASE WHEN length(CAST(status AS BLOB)) <= 32 THEN status END,
                CASE WHEN suspended_reason IS NULL THEN NULL
                     WHEN length(CAST(suspended_reason AS BLOB)) <= 32 THEN suspended_reason
                     ELSE 'invalid-oversized-reason' END
         FROM form_request WHERE status NOT IN ('answered', 'cancelled') AND session_id IS NOT NULL
           AND (?1 IS NULL OR session_id = ?1)
         ORDER BY rowid DESC LIMIT 129",
    )?;
    let mut rows = statement.query([session.map(SessionId::as_str)])?;
    let mut result = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next()? {
        if result.len() == MAX_FORM_CATALOG_ROWS {
            return Err(evidence(
                "unsettled forms exceed the 128-row response bound",
            ));
        }
        let id: FormId = row
            .get::<_, String>(0)?
            .parse()
            .map_err(|_| evidence("invalid form id"))?;
        let session: SessionId = row
            .get::<_, String>(1)?
            .parse()
            .map_err(|_| evidence("invalid session id"))?;
        let title: String = row.get(2)?;
        let status: String = row.get(3)?;
        let reason: Option<String> = row.get(4)?;
        let suspended = match (status.as_str(), reason.as_deref()) {
            ("pending", None) => None,
            ("suspended", Some("shutdown")) => Some(FormSuspensionReason::Shutdown),
            ("suspended", Some("process-loss")) => Some(FormSuspensionReason::ProcessLoss),
            _ => return Err(evidence("unsettled form has an invalid status/reason pair")),
        };
        let (info, size) = verified_creation_form(store, &session, &id, &title)?;
        bytes = bytes.saturating_add(size);
        if bytes > MAX_FORM_CATALOG_BYTES {
            return Err(evidence("form response exceeds its aggregate byte bound"));
        }
        result.push(PendingForm {
            id,
            session,
            info,
            suspended,
        });
    }
    Ok(result)
}

/// The creation event's form specification, verified against the projection row.
///
/// Shared by both reads so they cannot drift: a by-id read admitting a form on
/// weaker evidence than the list read would be a way in. Returns the form value
/// and the bytes it cost, so a caller accumulating an aggregate bound can.
fn verified_creation_form(
    store: &EventStore,
    session: &SessionId,
    id: &FormId,
    title: &str,
) -> Result<(Value, usize), FormCatalogError> {
    let mut events = store.connection().prepare(
        "SELECT length(CAST(data AS BLOB)),
                CASE WHEN length(CAST(data AS BLOB)) <= 262144 THEN data ELSE NULL END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
           AND type = 'form.created.1' AND json_extract(data, '$.form.id') = ?2
         ORDER BY seq LIMIT 2",
    )?;
    let mut events = events.query(rusqlite::params![session.as_str(), id.as_str()])?;
    let event = events
        .next()?
        .ok_or_else(|| evidence("creation event is missing"))?;
    let size = usize::try_from(event.get::<_, i64>(0)?)
        .map_err(|_| evidence("creation event byte length is invalid"))?;
    if size > MAX_FORM_EVENT_BYTES {
        return Err(evidence("creation event exceeds its byte bound"));
    }
    let raw: String = event.get(1)?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|_| evidence("creation event JSON is corrupt"))?;
    if events.next()?.is_some() {
        return Err(evidence("multiple creation events claim one form"));
    }
    match crate::FormEvent::decode("form.created", 1, &value)
        .map_err(|error| evidence(error.to_string()))?
    {
        Some(crate::FormEvent::Created {
            id: actual,
            session: owner,
            title: actual_title,
        }) if &actual == id && &owner == session && actual_title == title => {}
        _ => {
            return Err(evidence(
                "creation event disagrees with the form projection",
            ))
        }
    }
    let info = value
        .get("form")
        .cloned()
        .ok_or_else(|| evidence("creation event has no form"))?;
    if value.get("sessionID").and_then(Value::as_str) != Some(session.as_str()) {
        return Err(evidence(
            "creation event aggregate identity disagrees with its form",
        ));
    }
    if !info.get("fields").is_some_and(Value::is_array) {
        return Err(evidence("creation event has no field specification"));
    }
    Ok((info, size))
}

/// The settled-inclusive state of one form.
///
/// The projection above deliberately reads only UNSETTLED rows, because the
/// list surfaces it serves are "what is still blocking". A client that has just
/// replied asks a different question — what did this form end up as — and an
/// answered form must be able to hand its answer back. That is a read the
/// unsettled projection cannot serve by construction, so it gets its own.
#[derive(Debug, Clone, PartialEq)]
pub enum FormState {
    Pending,
    Suspended(FormSuspensionReason),
    /// Carries the recorded answer. Settlement is terminal, so this value does
    /// not change once written.
    Answered(Value),
    Cancelled,
}

impl FormState {
    /// The closed status vocabulary, matching the durable projection's.
    pub fn status_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Suspended(_) => "suspended",
            Self::Answered(_) => "answered",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One form read by identity, whatever state it is in.
#[derive(Debug, Clone, PartialEq)]
pub struct FormRecord {
    pub id: FormId,
    pub session: SessionId,
    pub info: Value,
    pub state: FormState,
}

/// Read one form by identity, including settled ones.
///
/// `None` means no such form for that session — absence, not an empty form.
/// Every other disagreement is corruption and refuses, exactly as the unsettled
/// projection does: the creation event is verified against the projection row,
/// Does any form already carry this identity, in ANY session?
///
/// A form id is unique across the whole projection, not per session, so a
/// creation check scoped to one session would miss a collision and then fail
/// deep inside the durable insert — a caller would be told its request was
/// malformed when the truth is that the id is taken.
pub fn form_id_exists(store: &EventStore, id: &FormId) -> Result<bool, FormCatalogError> {
    let mut statement = store
        .connection()
        .prepare("SELECT 1 FROM form_request WHERE id = ?1 LIMIT 1")?;
    let mut rows = statement.query(rusqlite::params![id.as_str()])?;
    Ok(rows.next()?.is_some())
}

/// a repeated creation event is refused, and the same per-event byte bound
/// applies. This borrows the existing connection and does not repair on read.
pub fn form_by_id(
    store: &EventStore,
    session: &SessionId,
    id: &FormId,
) -> Result<Option<FormRecord>, FormCatalogError> {
    let mut statement = store.connection().prepare(
        "SELECT CASE WHEN length(CAST(title AS BLOB)) <= 262144 THEN title END,
                CASE WHEN length(CAST(status AS BLOB)) <= 32 THEN status END,
                CASE WHEN suspended_reason IS NULL THEN NULL
                     WHEN length(CAST(suspended_reason AS BLOB)) <= 32 THEN suspended_reason
                     ELSE 'invalid-oversized-reason' END,
                CASE WHEN answer IS NULL THEN NULL
                     WHEN length(CAST(answer AS BLOB)) <= 262144 THEN answer
                     ELSE 'invalid-oversized-answer' END
         FROM form_request WHERE id = ?1 AND session_id = ?2 LIMIT 2",
    )?;
    let mut rows = statement.query(rusqlite::params![id.as_str(), session.as_str()])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let title: String = row.get(0)?;
    let status: String = row.get(1)?;
    let reason: Option<String> = row.get(2)?;
    let answer: Option<String> = row.get(3)?;
    let state = match (status.as_str(), reason.as_deref(), answer.as_deref()) {
        ("pending", None, None) => FormState::Pending,
        ("suspended", Some("shutdown"), None) => {
            FormState::Suspended(FormSuspensionReason::Shutdown)
        }
        ("suspended", Some("process-loss"), None) => {
            FormState::Suspended(FormSuspensionReason::ProcessLoss)
        }
        // A suspended form that was later answered keeps its reason; the
        // ANSWER is what settled it, so the settled state is what is reported.
        ("answered", _, Some(recorded)) => FormState::Answered(
            serde_json::from_str(recorded)
                .map_err(|_| evidence("recorded answer is not valid JSON"))?,
        ),
        ("cancelled", _, None) => FormState::Cancelled,
        // Every remaining combination contradicts the row's own status: an
        // answered form with no answer, a cancelled one carrying one, a
        // pending one that is somehow suspended. None becomes a default.
        _ => return Err(evidence("form status, reason and answer disagree")),
    };
    if rows.next()?.is_some() {
        return Err(evidence("two rows claim one form identity"));
    }
    let (info, _bytes) = verified_creation_form(store, session, id, &title)?;
    Ok(Some(FormRecord {
        id: id.clone(),
        session: session.clone(),
        info,
        state,
    }))
}

fn evidence(message: impl Into<String>) -> FormCatalogError {
    FormCatalogError::Evidence(message.into())
}
