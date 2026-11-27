//! Write-side Form blocker service.
//! Shares the blocker LAWS in `grapt_types::blocker` with the Permission
//! service but NOT an implementation.
//!
//! THE CONTRAST (contract §"Permission and Form blocker lifecycle"): form
//! suspension is NON-TERMINAL. A form may be answered or cancelled AFTER
//! suspension or a service replacement — there is NO `NotLive` refusal. So
//! `answer`/`cancel` accept a `Suspended` row (settling it and preserving the
//! `suspended_reason` as historical metadata), where the Permission service
//! would refuse a direct reply to a suspended request.
//!
//! Like Permission, every transition decides legality AND appends its durable
//! event + projects the `form_request` row inside ONE immediate transaction
//! via `append_within`.

use grapt_store::{
    AggregateId, AppendInput, EventStore, GovernedTransaction, HeldWriters, ProjectionId,
    Projector, WriteError,
};
use grapt_types::{CasOutcome, EventId, EventVersion, FormId, FormSuspensionReason, SessionId};
use rusqlite::Transaction;
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum FormError {
    #[error("global form: {0}")]
    Global(String),
    #[error(transparent)]
    Store(#[from] WriteError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("form {id}: durable record corrupt: {reason}")]
    Corruption { id: FormId, reason: String },
    #[error("form {id}: fields must be a JSON array")]
    InvalidFields { id: FormId },
    #[error("form {id}: presentation metadata must be an object of at most 4096 bytes")]
    InvalidPresentation { id: FormId },
    #[error("event version 1 must exist")]
    Version,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pending,
    Suspended,
    Answered,
    Cancelled,
}

/// Display information only. Ownership and settlement remain session scoped.
#[derive(Debug, Clone, Copy)]
pub struct FormPresentation<'a> {
    pub title: &'a str,
    pub fields: &'a Value,
    pub metadata: Option<&'a Value>,
}

impl Status {
    fn is_terminal(self) -> bool {
        matches!(self, Status::Answered | Status::Cancelled)
    }
}

fn reason_str(r: FormSuspensionReason) -> &'static str {
    match r {
        FormSuspensionReason::Shutdown => "shutdown",
        FormSuspensionReason::ProcessLoss => "process-loss",
    }
}

fn reason_from_str(raw: &str) -> Option<FormSuspensionReason> {
    match raw {
        "shutdown" => Some(FormSuspensionReason::Shutdown),
        "process-loss" => Some(FormSuspensionReason::ProcessLoss),
        _ => None,
    }
}

/// Session-scoped, exactly-one-row transition guard (review G).
fn affect_one(
    tx: &Transaction<'_>,
    sql: &str,
    params: impl rusqlite::Params,
    form: &FormId,
) -> Result<(), String> {
    let n = tx.execute(sql, params).map_err(|e| e.to_string())?;
    if n != 1 {
        return Err(format!(
            "form {form}: transition affected {n} rows (expected exactly 1; wrong session or unknown form)"
        ));
    }
    Ok(())
}

pub(crate) struct FormProjector;

impl Projector for FormProjector {
    fn project_form(
        &mut self,
        tx: &Transaction<'_>,
        event: &grapt_store::DecodedFormFact,
    ) -> Result<(), String> {
        crate::global::project(tx, event)
    }
    fn projection(&self) -> ProjectionId {
        ProjectionId::new("form")
    }

    fn project(
        &mut self,
        tx: &Transaction<'_>,
        event: &grapt_store::DecodedEvent,
    ) -> Result<(), String> {
        let decoded =
            crate::FormEvent::decode(event.kind.name(), event.kind.version().value(), &event.data)
                .map_err(|e| e.to_string())?;
        match decoded {
            Some(crate::FormEvent::Created { id, session, title }) => {
                tx.execute(
                    "INSERT INTO form_request (id, session_id, title, status, suspended_reason, answer)
                     VALUES (?1, ?2, ?3, 'pending', NULL, NULL)",
                    rusqlite::params![id.as_str(), session.as_str(), title],
                )
                .map_err(|e| e.to_string())?;
            }
            Some(crate::FormEvent::Suspended {
                form,
                session,
                reason,
            }) => {
                affect_one(
                    tx,
                    "UPDATE form_request SET status = 'suspended', suspended_reason = ?3
                     WHERE id = ?1 AND session_id = ?2",
                    rusqlite::params![form.as_str(), session.as_str(), reason_str(reason)],
                    &form,
                )?;
            }
            Some(crate::FormEvent::Answered {
                form,
                session,
                answer,
            }) => {
                // Keep suspended_reason as history if the form was suspended.
                let answer_text = serde_json::to_string(&answer).map_err(|e| e.to_string())?;
                affect_one(
                    tx,
                    "UPDATE form_request SET status = 'answered', answer = ?3
                     WHERE id = ?1 AND session_id = ?2",
                    rusqlite::params![form.as_str(), session.as_str(), answer_text],
                    &form,
                )?;
            }
            Some(crate::FormEvent::Cancelled { form, session }) => {
                affect_one(
                    tx,
                    "UPDATE form_request SET status = 'cancelled'
                     WHERE id = ?1 AND session_id = ?2",
                    rusqlite::params![form.as_str(), session.as_str()],
                    &form,
                )?;
            }
            None => {}
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct FormService {
    store: EventStore,
}

struct Row {
    session: Option<String>,
    global_binding: Option<String>,
    status: Status,
}

type StoredFormRow = (Option<String>, String, Option<String>, Option<String>);

impl FormService {
    pub fn new(mut store: EventStore) -> Result<Self, FormError> {
        Self::initialize(&mut store)?;
        Ok(Self { store })
    }

    /// Compose the form owner on a store held by the product's inbox.
    pub fn initialize(store: &mut EventStore) -> Result<(), FormError> {
        // Read-only first: a current schema needs no transaction at all. Any
        // creation or upgrade then happens in ONE governed transaction, so a
        // fresh database pays one durable commit instead of one per statement.
        if Self::schema_is_current(store.connection())? {
            return Ok(());
        }
        let tx = store.governed_transaction()?;
        Self::initialize_within(&tx)?;
        tx.commit()?.discard();
        Ok(())
    }

    /// The schema creation or upgrade of [`Self::initialize`], inside a
    /// transaction the caller holds, so a composition can install several
    /// owners' schemas in one commit.
    pub fn initialize_within(tx: &Transaction<'_>) -> Result<(), FormError> {
        if Self::schema_is_current(tx)? {
            return Ok(());
        }
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS form_request (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                title TEXT NOT NULL,
                status TEXT NOT NULL,
                suspended_reason TEXT,
                answer TEXT
            );",
        )?;
        if !Self::schema_is_current(tx)? {
            tx.execute_batch(
                "CREATE TABLE form_request_global_upgrade (
                    id TEXT PRIMARY KEY, session_id TEXT, title TEXT NOT NULL,
                    status TEXT NOT NULL, suspended_reason TEXT, answer TEXT,
                    global_binding TEXT,
                    CHECK ((session_id IS NOT NULL AND global_binding IS NULL)
                        OR (session_id IS NULL AND global_binding IS NOT NULL))
                );
                INSERT INTO form_request_global_upgrade
                    (id, session_id, title, status, suspended_reason, answer)
                    SELECT id, session_id, title, status, suspended_reason, answer FROM form_request;
                DROP TABLE form_request;
                ALTER TABLE form_request_global_upgrade RENAME TO form_request;"
            )?;
        }
        Ok(())
    }

    /// Whether `form_request` exists in its current (global-binding) shape.
    fn schema_is_current(conn: &rusqlite::Connection) -> Result<bool, FormError> {
        let mut statement = conn.prepare("PRAGMA table_info(form_request)")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        let columns = rows.collect::<Result<Vec<_>, _>>()?;
        Ok(columns.iter().any(|name| name == "global_binding"))
    }

    pub fn store(&self) -> &EventStore {
        &self.store
    }

    fn read_row(tx: &Transaction<'_>, id: &FormId) -> Result<Option<Row>, FormError> {
        let raw: Option<StoredFormRow> = tx
            .query_row(
                "SELECT CASE WHEN length(CAST(session_id AS BLOB)) <= 512 THEN session_id END,
                    CASE WHEN length(status) <= 16 THEN status END,
                    CASE WHEN suspended_reason IS NULL THEN NULL WHEN length(suspended_reason) <= 16 THEN suspended_reason ELSE '!oversized!' END,
                    CASE WHEN global_binding IS NULL THEN NULL WHEN length(CAST(global_binding AS BLOB)) <= 8192 THEN global_binding ELSE '!oversized!' END
                 FROM form_request WHERE id = ?1",
                [id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        let Some((session, status_raw, reason_raw, global_binding)) = raw else {
            return Ok(None);
        };
        let corrupt = |reason: String| FormError::Corruption {
            id: id.clone(),
            reason,
        };
        let status = match status_raw.as_str() {
            "pending" => Status::Pending,
            "suspended" => Status::Suspended,
            "answered" => Status::Answered,
            "cancelled" => Status::Cancelled,
            other => {
                return Err(corrupt(format!(
                    "status {other:?} outside the closed vocabulary"
                )))
            }
        };
        // Reason-required law: a suspended row without a reason is corrupt.
        let has_reason = match reason_raw {
            Some(r) => {
                reason_from_str(&r).ok_or_else(|| {
                    corrupt(format!("suspended_reason {r:?} outside the vocabulary"))
                })?;
                true
            }
            None => false,
        };
        if status == Status::Suspended && !has_reason {
            return Err(corrupt("suspended row without a reason".into()));
        }
        if session.is_some() == global_binding.is_some() {
            return Err(corrupt("form owner is absent or ambiguous".into()));
        }
        Ok(Some(Row {
            session,
            status,
            global_binding,
        }))
    }

    fn append(
        tx: &GovernedTransaction<'_>,
        held: &HeldWriters,
        session: &SessionId,
        event_id: &EventId,
        kind: &str,
        data: &Value,
        created_ms: i64,
    ) -> Result<(), FormError> {
        let version = EventVersion::new(1).ok_or(FormError::Version)?;
        // The writer epoch never enters this service's public signatures: the
        // token comes from the store this service already owns, snapshotted
        // before the transaction because the session is only known after the
        // row read inside it.
        let aggregate = AggregateId::Session(session.clone());
        let writer = held.token(&aggregate)?;
        EventStore::append_within(
            tx,
            AppendInput {
                id: event_id,
                aggregate,
                writer,
                kind_name: kind,
                version,
                created_ms,
                data,
            },
            &mut [&mut FormProjector],
        )?;
        Ok(())
    }

    /// Create (issue) a form: one created event + a pending row. `fields` is
    /// the real form field spec and is carried verbatim into the durable event
    /// (review F) — it must be a JSON array; an empty array is only valid when
    /// the form genuinely has no fields.
    pub fn create(
        &mut self,
        event_id: &EventId,
        id: &FormId,
        session: &SessionId,
        title: &str,
        fields: &Value,
        created_ms: i64,
    ) -> Result<(), FormError> {
        Self::create_on(
            &mut self.store,
            event_id,
            id,
            session,
            title,
            fields,
            created_ms,
        )
    }

    /// The same creation against a store this owner does not hold.
    ///
    /// The product's inbox owns one store for every durable owner, so a caller
    /// there cannot hand ownership to a `FormService`. Both entry points run
    /// the identical body — this is not a second, weaker creation path.
    pub fn create_on(
        store: &mut EventStore,
        event_id: &EventId,
        id: &FormId,
        session: &SessionId,
        title: &str,
        fields: &Value,
        created_ms: i64,
    ) -> Result<(), FormError> {
        Self::create_presented_on(
            store,
            event_id,
            id,
            session,
            FormPresentation {
                title,
                fields,
                metadata: None,
            },
            created_ms,
        )
    }

    /// Creation with optional display metadata through the same transaction owner.
    pub fn create_presented_on(
        store: &mut EventStore,
        event_id: &EventId,
        id: &FormId,
        session: &SessionId,
        presentation: FormPresentation<'_>,
        created_ms: i64,
    ) -> Result<(), FormError> {
        let FormPresentation {
            title,
            fields,
            metadata,
        } = presentation;
        if !fields.is_array() {
            return Err(FormError::InvalidFields { id: id.clone() });
        }
        if metadata.is_some_and(|value| !value.is_object() || value.to_string().len() > 4096) {
            return Err(FormError::InvalidPresentation { id: id.clone() });
        }
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        let mut data = json!({
            "form": {
                "id": id.as_str(),
                "sessionID": session.as_str(),
                "title": title,
                "fields": fields,
            },
            "sessionID": session.as_str(),
        });
        if let Some(metadata) = metadata {
            data["form"]["metadata"] = metadata.clone();
        }
        Self::append(
            &tx,
            &held,
            session,
            event_id,
            "form.created",
            &data,
            created_ms,
        )?;
        tx.commit()?.discard();
        Ok(())
    }

    /// Suspend a live (pending) form. A form already suspended or terminal is
    /// not re-suspended.
    pub fn suspend(
        &mut self,
        event_id: &EventId,
        id: &FormId,
        reason: FormSuspensionReason,
        created_ms: i64,
    ) -> Result<CasOutcome, FormError> {
        Self::suspend_on(&mut self.store, event_id, id, reason, created_ms)
    }

    pub fn suspend_on(
        store: &mut EventStore,
        event_id: &EventId,
        id: &FormId,
        reason: FormSuspensionReason,
        created_ms: i64,
    ) -> Result<CasOutcome, FormError> {
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        let outcome = match Self::read_row(&tx, id)? {
            None => CasOutcome::NotLive,
            Some(row) if row.status.is_terminal() || row.status == Status::Suspended => {
                CasOutcome::AlreadyTerminal
            }
            Some(row) => {
                if let Some(raw) = &row.global_binding {
                    let binding: Value = serde_json::from_str(raw)
                        .map_err(|error| FormError::Global(error.to_string()))?;
                    crate::global::GlobalFormBinding::decode(&binding)
                        .map_err(FormError::Global)?;
                    let data =
                        json!({"formID":id.as_str(), "owner":binding, "reason":reason_str(reason)});
                    Self::append_global(
                        &tx,
                        &held,
                        id,
                        event_id,
                        "form.suspended",
                        &data,
                        created_ms,
                    )?;
                    tx.commit()?.discard();
                    return Ok(CasOutcome::Applied);
                }
                let session = Self::session_of(id, &row)?;
                let data = json!({
                    "sessionID": session.as_str(),
                    "formID": id.as_str(),
                    "reason": reason_str(reason),
                });
                Self::append(
                    &tx,
                    &held,
                    &session,
                    event_id,
                    "form.suspended",
                    &data,
                    created_ms,
                )?;
                CasOutcome::Applied
            }
        };
        tx.commit()?.discard();
        Ok(outcome)
    }

    /// Answer a form. THE CONTRAST: a pending OR suspended form answers
    /// successfully; only a terminal row refuses. Suspension reason is kept as
    /// history.
    pub fn answer(
        &mut self,
        event_id: &EventId,
        id: &FormId,
        answer: &Value,
        created_ms: i64,
    ) -> Result<CasOutcome, FormError> {
        Self::answer_on(&mut self.store, event_id, id, answer, created_ms)
    }

    /// The same answer against a store this owner does not hold.
    pub fn answer_on(
        store: &mut EventStore,
        event_id: &EventId,
        id: &FormId,
        answer: &Value,
        created_ms: i64,
    ) -> Result<CasOutcome, FormError> {
        Self::settle_on(
            store,
            event_id,
            id,
            "form.replied",
            |session| json!({"id": id.as_str(), "sessionID": session, "answer": answer}),
            created_ms,
            None,
        )
    }

    /// Cancel a form. Like answer, a pending OR suspended form cancels; only a
    /// terminal row refuses.
    pub fn cancel(
        &mut self,
        event_id: &EventId,
        id: &FormId,
        created_ms: i64,
    ) -> Result<CasOutcome, FormError> {
        Self::cancel_on(&mut self.store, event_id, id, created_ms)
    }

    /// The same cancellation against a store this owner does not hold.
    pub fn cancel_on(
        store: &mut EventStore,
        event_id: &EventId,
        id: &FormId,
        created_ms: i64,
    ) -> Result<CasOutcome, FormError> {
        Self::settle_on(
            store,
            event_id,
            id,
            "form.cancelled",
            |session| json!({"id": id.as_str(), "sessionID": session}),
            created_ms,
            None,
        )
    }

    pub(crate) fn settle_on(
        store: &mut EventStore,
        event_id: &EventId,
        id: &FormId,
        kind: &str,
        data_for: impl FnOnce(&str) -> Value,
        created_ms: i64,
        expected: Option<&crate::global::GlobalFormBinding>,
    ) -> Result<CasOutcome, FormError> {
        let held = store.held_writers();
        let tx = store.governed_transaction()?;
        let outcome = match Self::read_row(&tx, id)? {
            None => CasOutcome::NotLive,
            // Pending OR Suspended: both settle — the Permission contrast.
            Some(row) => {
                if let Some(raw) = &row.global_binding {
                    let binding: Value = serde_json::from_str(raw)
                        .map_err(|error| FormError::Global(error.to_string()))?;
                    let decoded = crate::global::GlobalFormBinding::decode(&binding)
                        .map_err(FormError::Global)?;
                    // COMPATIBILITY, NOT EQUALITY — see `GlobalFormBinding::compatible_with`.
                    // The ownership coordinates must match; the observational
                    // dispatch must not contradict. Comparing whole structs
                    // made a form created before the dispatch field existed
                    // unsettleable under code that now carries one, which a
                    // test probed as a real "binding mismatch" refusal.
                    if !expected.is_some_and(|expected| decoded.compatible_with(expected)) {
                        return Err(FormError::Global(
                            "global form access binding mismatch".into(),
                        ));
                    }
                    if row.status.is_terminal() {
                        return Ok(CasOutcome::AlreadyTerminal);
                    }
                    let mut data = data_for("global");
                    data.as_object_mut()
                        .ok_or_else(|| FormError::Global("settlement object absent".into()))?
                        .remove("sessionID");
                    data["formID"] = json!(id.as_str());
                    data["owner"] = binding;
                    Self::append_global(&tx, &held, id, event_id, kind, &data, created_ms)?;
                    tx.commit()?.discard();
                    return Ok(CasOutcome::Applied);
                }
                if expected.is_some() {
                    return Err(FormError::Global(
                        "global access cannot settle a Session form".into(),
                    ));
                }
                if row.status.is_terminal() {
                    return Ok(CasOutcome::AlreadyTerminal);
                }
                let session = Self::session_of(id, &row)?;
                Self::append(
                    &tx,
                    &held,
                    &session,
                    event_id,
                    kind,
                    &data_for(session.as_str()),
                    created_ms,
                )?;
                CasOutcome::Applied
            }
        };
        tx.commit()?.discard();
        Ok(outcome)
    }

    fn session_of(id: &FormId, row: &Row) -> Result<SessionId, FormError> {
        row.session
            .as_deref()
            .ok_or_else(|| FormError::Global("form has no Session owner".into()))?
            .parse()
            .map_err(|e: grapt_types::IdError| FormError::Corruption {
                id: id.clone(),
                reason: e.to_string(),
            })
    }
}
