//! Session names use the existing lifecycle event and list projection.
//! Automatic naming is a separate commit after human prompt admission.
use crate::{SessionListProjector, SessionStore, SessionStoreError};
use grapt_store::{AggregateId, AppendInput, Appended, EventStore, WriteError};
use grapt_types::{DeterministicAuthorities, EventId, EventVersion, SessionId};
use serde_json::{json, Value};

pub const DEFAULT_TITLE: &str = "New session";
pub const MAX_TITLE_CHARACTERS: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum TitleError {
    #[error("title must be a nonempty printable line of at most 100 characters")]
    Invalid,
    #[error(transparent)]
    Read(#[from] SessionStoreError),
    #[error(transparent)]
    Write(#[from] WriteError),
    #[error("title event identity failed: {0}")]
    Identity(#[from] grapt_types::IdError),
    #[error("title event identity repeated")]
    RepeatedEvent,
}

/// Describe the first nonempty human prompt line without a provider call.
pub fn prompt_title(prompt: &str) -> Option<String> {
    for line in prompt.lines() {
        let mut title = String::new();
        let mut space = false;
        let mut count = 0;
        for ch in line.chars() {
            if ch.is_whitespace() {
                space = !title.is_empty();
                continue;
            }
            if ch.is_control() {
                continue;
            }
            if space {
                title.push(' ');
                count += 1;
                space = false;
            }
            title.push(ch);
            count += 1;
            if count > MAX_TITLE_CHARACTERS {
                title = title.chars().take(MAX_TITLE_CHARACTERS - 3).collect();
                title.push_str("...");
                return Some(title);
            }
        }
        if !title.is_empty() {
            return Some(title);
        }
    }
    None
}

pub fn rename_session(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    title: &str,
) -> Result<Option<u64>, TitleError> {
    let title = title.trim();
    if title.is_empty()
        || title.chars().count() > MAX_TITLE_CHARACTERS
        || title.chars().any(char::is_control)
    {
        return Err(TitleError::Invalid);
    }
    rename(store, authorities, session, Some(title))
}

pub fn name_from_first_prompt(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
) -> Result<Option<u64>, TitleError> {
    rename(store, authorities, session, None)
}

fn rename(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    manual: Option<&str>,
) -> Result<Option<u64>, TitleError> {
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let tx = store.governed_transaction().map_err(WriteError::from)?;
    let current = SessionStore::new(&tx).record(session)?;
    let read_error = |source| SessionStoreError::Store {
        session: session.as_str().to_owned(),
        source,
    };
    let renamed: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM event WHERE aggregate_family = 'session'
         AND aggregate_id = ?1 AND type = 'session.renamed.1')",
            [session.as_str()],
            |row| row.get(0),
        )
        .map_err(read_error)?;
    if manual.is_none() && (current.parent.is_some() || current.title != DEFAULT_TITLE || renamed) {
        return Ok(None);
    }
    let title = match manual {
        Some(title) => title.to_owned(),
        None => {
            // Admission, not promotion, defines the first human input. A
            // crash before naming can therefore retry from the same input.
            let Some(title) = first_human_title(&tx, session)? else {
                return Ok(None);
            };
            title
        }
    };
    // A first explicit rename to the default text still counts as a manual
    // name. Omitting that event would allow a later automatic overwrite.
    if current.title == title && renamed {
        return Ok(None);
    }
    let event: EventId = format!("evt_{}", authorities.mint().as_str()).parse()?;
    let version = EventVersion::new(1).ok_or(TitleError::Invalid)?;
    let data = json!({"sessionID":session.as_str(),"title":title});
    let appended = EventStore::append_within(
        &tx,
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.renamed",
            version,
            created_ms: authorities.now().as_millis(),
            data: &data,
        },
        &mut [&mut SessionListProjector],
    )?;
    let seq = match appended {
        Appended::Committed { seq } => seq,
        Appended::Idempotent { .. } => return Err(TitleError::RepeatedEvent),
    };
    tx.commit().map_err(WriteError::from)?.discard();
    Ok(Some(seq))
}

fn first_human_title(
    tx: &rusqlite::Transaction<'_>,
    session: &SessionId,
) -> Result<Option<String>, TitleError> {
    let read_error = |source| SessionStoreError::Store {
        session: session.as_str().to_owned(),
        source,
    };
    let corrupt = |reason: String| SessionStoreError::Corrupt {
        session: session.as_str().to_owned(),
        reason,
    };
    // Synthetic admissions share the kind with human admissions. Inspect a
    // bounded canonical prefix, rather than naming a synthetic task or
    // trusting a rebuildable pending queue as the first-prompt authority.
    let mut statement = tx
        .prepare(
            "SELECT CASE WHEN length(CAST(data AS BLOB)) <= 1048576 THEN data END
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         AND type = 'session.input.admitted.1' ORDER BY seq LIMIT ?2",
        )
        .map_err(read_error)?;
    let limit =
        i64::try_from(crate::MAX_HISTORY_ROWS + 1).map_err(|error| corrupt(error.to_string()))?;
    let mut rows = statement
        .query((session.as_str(), limit))
        .map_err(read_error)?;
    for index in 0..=crate::MAX_HISTORY_ROWS {
        let Some(row) = rows.next().map_err(read_error)? else {
            return Ok(None);
        };
        if index == crate::MAX_HISTORY_ROWS {
            return Err(
                corrupt("first human title exceeds the bounded admission prefix".into()).into(),
            );
        }
        let raw: String = row.get(0).map_err(read_error)?;
        let data: Value = serde_json::from_str(&raw).map_err(|error| corrupt(error.to_string()))?;
        if data.get("sessionID").and_then(Value::as_str) != Some(session.as_str()) {
            return Err(corrupt("admission has invalid Session identity".into()).into());
        }
        match data.pointer("/input/type").and_then(Value::as_str) {
            Some("synthetic") => continue,
            Some("user") => {
                let prompt = data
                    .pointer("/input/data/text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| corrupt("first human admission text is absent".into()))?;
                if data
                    .pointer("/input/data/metadata/origin")
                    .and_then(Value::as_str)
                    == Some("seat-startup")
                {
                    continue;
                }
                return Ok(prompt_title(prompt));
            }
            _ => return Err(corrupt("admission has invalid input type".into()).into()),
        }
    }
    Ok(None)
}
