use crate::key::SubmitKey;
use crate::state::{Event, RuntimeSnapshot, SessionRuntimeState, StateSeq};
use grapt_run_store::{
    Command, Durability, ReadError, ReadPool, ReadView, RequestKey, Scope, Statement, Value,
    WriteError,
};
use grapt_types::SessionId;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

/// Tables this crate writes. The run store's owner includes them in the
/// schema it opens the database with.
pub const SCHEMA: &str = "
CREATE TABLE session_runtime (
    session TEXT PRIMARY KEY,
    state TEXT NOT NULL,
    state_seq INTEGER NOT NULL
);
CREATE TABLE session_transition (
    session TEXT NOT NULL,
    state_seq INTEGER NOT NULL,
    submit_key TEXT NOT NULL,
    event TEXT NOT NULL,
    state TEXT NOT NULL,
    PRIMARY KEY (session, state_seq),
    UNIQUE (session, submit_key)
);
";

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error(transparent)]
    Read(#[from] ReadError),
    #[error("stored runtime state for this session is malformed")]
    Malformed,
}

/// The committed snapshot of `session`: `Idle` at sequence zero when the
/// session has never transitioned.
pub async fn load(
    readers: &ReadPool,
    session: SessionId,
    deadline: Instant,
) -> Result<RuntimeSnapshot, LoadError> {
    let id = session.as_str().to_owned();
    let row = readers
        .snapshot(deadline, move |read| runtime_row(read, &id))
        .await?;
    snapshot_from(session, row)
}

/// What a submit key already committed in this session, if anything.
pub(crate) struct Recorded {
    pub(crate) event: String,
    pub(crate) state: SessionRuntimeState,
    pub(crate) state_seq: StateSeq,
}

/// The submit key's transition record and the session snapshot, read from
/// one database snapshot so the two always agree.
pub(crate) async fn recorded(
    readers: &ReadPool,
    session: &SessionId,
    key: &SubmitKey,
    deadline: Instant,
) -> Result<(Option<Recorded>, RuntimeSnapshot), LoadError> {
    let (id, submit) = (session.as_str().to_owned(), key.as_str().to_owned());
    let (record, runtime) = readers
        .snapshot(deadline, move |read| {
            Ok((recorded_row(read, &id, &submit)?, runtime_row(read, &id)?))
        })
        .await?;
    let snapshot = snapshot_from(session.clone(), runtime)?;
    let Some((event, state, seq)) = record else {
        return Ok((None, snapshot));
    };
    match (
        SessionRuntimeState::parse(&state),
        StateSeq::from_stored(seq),
    ) {
        (Some(state), Some(state_seq)) if state_seq <= snapshot.state_seq => Ok((
            Some(Recorded {
                event,
                state,
                state_seq,
            }),
            snapshot,
        )),
        _ => Err(LoadError::Malformed),
    }
}

fn runtime_row(read: &ReadView<'_>, session: &str) -> Result<Option<(String, i64)>, ReadError> {
    let mut statement =
        read.prepare("SELECT state, state_seq FROM session_runtime WHERE session = ?1")?;
    let mut rows = statement.query([session])?;
    Ok(match rows.next()? {
        Some(row) => Some((row.get(0)?, row.get(1)?)),
        None => None,
    })
}

fn snapshot_from(
    session: SessionId,
    row: Option<(String, i64)>,
) -> Result<RuntimeSnapshot, LoadError> {
    let Some((state, seq)) = row else {
        return Ok(RuntimeSnapshot {
            session,
            state: SessionRuntimeState::Idle,
            state_seq: StateSeq::INITIAL,
        });
    };
    match (
        SessionRuntimeState::parse(&state),
        StateSeq::from_stored(seq),
    ) {
        (Some(state), Some(state_seq)) if state_seq > StateSeq::INITIAL => Ok(RuntimeSnapshot {
            session,
            state,
            state_seq,
        }),
        _ => Err(LoadError::Malformed),
    }
}

fn recorded_row(
    read: &ReadView<'_>,
    session: &str,
    submit: &str,
) -> Result<Option<(String, String, i64)>, ReadError> {
    let mut statement = read.prepare(
        "SELECT event, state, state_seq FROM session_transition \
         WHERE session = ?1 AND submit_key = ?2",
    )?;
    let mut rows = statement.query([session, submit])?;
    Ok(match rows.next()? {
        Some(row) => Some((row.get(0)?, row.get(1)?, row.get(2)?)),
        None => None,
    })
}

/// One atomic command: compare-and-set the session row on its previous
/// sequence, and record the transition under its submit key.
pub(crate) fn transition(
    from: &RuntimeSnapshot,
    key: &SubmitKey,
    event: Event,
    to: SessionRuntimeState,
    next: StateSeq,
) -> Result<Command, WriteError> {
    let session = || Value::Text(from.session.as_str().to_owned());
    let seq = |seq: StateSeq| Value::Integer(seq.stored());
    let state = Value::Text(to.as_str().to_owned());
    let runtime = if from.state_seq == StateSeq::INITIAL {
        Statement::new(
            "INSERT INTO session_runtime (session, state, state_seq) VALUES (?1, ?2, ?3) \
             ON CONFLICT (session) DO NOTHING",
            vec![session(), state.clone(), seq(next)],
        )
    } else {
        Statement::new(
            "UPDATE session_runtime SET state = ?2, state_seq = ?3 \
             WHERE session = ?1 AND state_seq = ?4",
            vec![session(), state.clone(), seq(next), seq(from.state_seq)],
        )
    };
    let record = Statement::new(
        "INSERT INTO session_transition (session, state_seq, submit_key, event, state) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        vec![
            session(),
            seq(next),
            Value::Text(key.as_str().to_owned()),
            Value::Text(event.as_str().to_owned()),
            state,
        ],
    );
    Command::new(
        request_key(&from.session, key)?,
        Durability::Full,
        Scope::Session(from.session.clone()),
        vec![runtime.requiring_rows(1), record.requiring_rows(1)],
    )
}

/// Request keys are global to the store, so each is derived from both the
/// session and the caller's key: equal submit keys in two sessions differ.
fn request_key(session: &SessionId, key: &SubmitKey) -> Result<RequestKey, WriteError> {
    let mut hash = Sha256::new();
    for part in [session.as_str(), key.as_str()] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    let digest = hash.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(RequestKey::new(&format!("session-actor-{hex}"))?)
}
