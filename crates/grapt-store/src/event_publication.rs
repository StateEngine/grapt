//! Bounded committed facts used by live event publishers.
//!
//! These reads preserve raw storage values for the caller's strict domain decoder.
use crate::EventStore;
use grapt_types::{EventId, FormId, SessionId, StepNumber, ToolCallId};

/// The bounded input fact at one durable session position.
#[derive(Debug)]
pub struct InputFact {
    pub id: String,
    pub kind: String,
    pub created: i64,
    pub data: String,
}

pub fn input_fact(store: &EventStore, session: &SessionId, seq: u64) -> Result<InputFact, String> {
    let stored_seq = i64::try_from(seq).map_err(|error| error.to_string())?;
    store
        .connection()
        .query_row(
            "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 256 THEN id END,
                CASE WHEN length(CAST(type AS BLOB)) <= 128 THEN type END,
                created, CASE WHEN length(CAST(data AS BLOB)) <= 1048576 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND seq = ?2",
            (session.as_str(), stored_seq),
            |row| {
                Ok(InputFact {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    created: row.get(2)?,
                    data: row.get(3)?,
                })
            },
        )
        .map_err(|error| error.to_string())
}

/// Missing is legal when a configured permission emits no new ask.
pub fn input_sequence(
    store: &EventStore,
    session: &SessionId,
    event: &EventId,
) -> Result<Option<i64>, rusqlite::Error> {
    store.connection().query_row(
        "SELECT (SELECT seq FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND id = ?2)",
        (session.as_str(), event.as_str()), |row| row.get(0),
    )
}

/// A bounded global form fact, before domain and wire decoding.
#[derive(Debug)]
pub struct FormFact {
    pub seq: i64,
    pub created: i64,
    pub kind: String,
    pub data: String,
}

pub fn form_fact(store: &EventStore, form: &FormId, event: &EventId) -> Result<FormFact, String> {
    store
        .connection()
        .query_row(
            "SELECT seq, created, CASE WHEN length(type) <= 128 THEN type END,
            CASE WHEN length(CAST(data AS BLOB)) <= 262144 THEN data END
         FROM event WHERE aggregate_family='form' AND aggregate_id=?1 AND id=?2",
            (form.as_str(), event.as_str()),
            |row| {
                Ok(FormFact {
                    seq: row.get(0)?,
                    created: row.get(1)?,
                    kind: row.get(2)?,
                    data: row.get(3)?,
                })
            },
        )
        .map_err(|error| error.to_string())
}

/// One assistant opening, with the original duplicate and missing-row refusals.
#[derive(Debug)]
pub struct AssistantOpening {
    pub id: String,
    pub created: i64,
    pub seq: i64,
    pub data: String,
    pub kind: String,
}

pub fn assistant_opening(
    store: &EventStore,
    session: &SessionId,
    step: StepNumber,
) -> Result<AssistantOpening, String> {
    let mut statement = store
        .connection()
        .prepare(
            "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 256 THEN id END,
            created, seq, CASE WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END, type
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
           AND type IN ('session.assistant-message.opened.1', 'session.assistant-message.opened.2')
           AND json_extract(data, '$.step') = ?2 ORDER BY seq LIMIT 2",
        )
        .map_err(|error| error.to_string())?;
    let mut rows = statement
        .query((session.as_str(), i64::from(step.value())))
        .map_err(|error| error.to_string())?;
    let row = rows
        .next()
        .map_err(|error| error.to_string())?
        .ok_or("Step has no committed assistant opening")?;
    let opening = AssistantOpening {
        id: row.get(0).map_err(|error| error.to_string())?,
        created: row.get(1).map_err(|error| error.to_string())?,
        seq: row.get(2).map_err(|error| error.to_string())?,
        data: row.get(3).map_err(|error| error.to_string())?,
        kind: row.get(4).map_err(|error| error.to_string())?,
    };
    if rows.next().map_err(|error| error.to_string())?.is_some() {
        return Err("Step has multiple assistant openings".into());
    }
    Ok(opening)
}

/// A tool's unique committed event identity and position.
#[derive(Debug)]
pub struct ToolPosition {
    pub event: EventId,
    pub seq: u64,
    pub created_ms: i64,
}

pub fn tool_position(
    store: &EventStore,
    session: &SessionId,
    call: &ToolCallId,
    kind: &str,
    second_kind: Option<&str>,
    path: &str,
) -> Result<ToolPosition, String> {
    let terminal = crate::stored_terminal::bounded_column("data");
    let mut statement = store.connection().prepare(&format!(
        "SELECT CASE WHEN length(id) <= 256 THEN id ELSE NULL END, seq, created,
                CASE WHEN type = 'session.tool.terminal.1' THEN {terminal} END FROM event WHERE aggregate_family = 'session'
             AND aggregate_id = ?1 AND type IN (?2, ?3) AND json_valid(data) AND json_extract(data, ?4) = ?5 ORDER BY seq"
    )).map_err(|e|e.to_string())?;
    let mut rows = statement
        .query((session.as_str(), kind, second_kind, path, call.as_str()))
        .map_err(|e| e.to_string())?;
    // A terminal row the decoder refuses is no commit of this call: it is
    // neither the position nor a duplicate of it.
    let mut found = None;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let terminal =
            crate::stored_terminal::classify_optional(row, 3).map_err(|e| e.to_string())?;
        if matches!(
            terminal,
            Some(grapt_event::session_tool_terminal::StoredTerminal::Unreadable { .. })
        ) {
            continue;
        }
        if found.is_some() {
            return Err("tool commit has invalid time or duplicate identity".into());
        }
        let id: String = row.get(0).map_err(|e| e.to_string())?;
        let seq: i64 = row.get(1).map_err(|e| e.to_string())?;
        let created: i64 = row.get(2).map_err(|e| e.to_string())?;
        found = Some((id, seq, created));
    }
    let (id, seq, created) = found.ok_or("tool commit is missing")?;
    if created < 0 {
        return Err("tool commit has invalid time or duplicate identity".into());
    }
    Ok(ToolPosition {
        event: id
            .parse()
            .map_err(|e: grapt_types::IdError| e.to_string())?,
        seq: u64::try_from(seq).map_err(|e| e.to_string())?,
        created_ms: created,
    })
}
