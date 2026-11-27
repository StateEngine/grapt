//! A turn the service stopped on purpose, and whether the service may continue
//! it on its own.
//!
//! A user's stop is theirs and stays stopped. A service shutdown is not a
//! decision about the work, so the turn that was running when the service went
//! away is recorded here as resumable, exactly once, at the instant the
//! shutdown cause is known. The reader that continues it needs nothing from
//! that process: the record names the Session's own latest input, so the
//! service that comes back can offer the same turn again instead of leaving the
//! seat idle.
use crate::notice::{append_in, kind_rows_at, matching_rows, MAX_NOTICES};
use crate::{CommittedPosition, SessionStore};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{DeterministicAuthorities, ExecutionId, MessageId, SessionId};
use serde::{Deserialize, Serialize};

pub(crate) const INTERRUPTED_TURN: &str = "interrupted-turn";

/// Why the service stopped the turn, in the vocabulary a later build reads.
/// A reason this build does not know is retained as [`Resume::Unsupported`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoppedBy {
    /// The service application's lifetime ended; it is expected to come back.
    ServiceShutdown,
    /// A disposable serve window ended.
    ServeWindowExpired,
}

impl StoppedBy {
    fn word(self) -> &'static str {
        match self {
            Self::ServiceShutdown => "service-shutdown",
            Self::ServeWindowExpired => "serve-window-expired",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        match word {
            "service-shutdown" => Some(Self::ServiceShutdown),
            "serve-window-expired" => Some(Self::ServeWindowExpired),
            _ => None,
        }
    }
}

/// What a reader may do with the recorded turn.
///
/// The distinction is the whole safety story: a turn whose durable history a
/// fresh request can rebuild is continued, and a turn whose history is not
/// provably complete (an unsettled tool call, an unreadable row) is only
/// announced, never replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resume {
    /// The turn can be continued from its last durable point.
    Resumable,
    /// The turn must not be continued automatically; the Session shows why.
    Unsupported,
}

impl Resume {
    fn word(self) -> &'static str {
        match self {
            Self::Resumable => "resumable",
            Self::Unsupported => "unsupported",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        match word {
            "resumable" => Some(Self::Resumable),
            "unsupported" => Some(Self::Unsupported),
            _ => None,
        }
    }
}

/// One recorded service stop of a running turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptedTurn {
    pub position: CommittedPosition,
    pub execution: ExecutionId,
    /// The Session's latest promoted input: the turn a continuation answers.
    pub input: MessageId,
    pub by: StoppedBy,
    pub resume: Resume,
}

impl InterruptedTurn {
    /// The one line the Session shows. It names what stopped the turn and the
    /// action that actually continues it, or says plainly that this turn
    /// cannot be continued on its own.
    pub fn text(&self) -> String {
        let stopped = match self.by {
            StoppedBy::ServiceShutdown => "The service stopped",
            StoppedBy::ServeWindowExpired => "The session window ended",
        };
        match self.resume {
            Resume::Resumable => format!(
                "{stopped} while this turn was running. Send a follow-up to continue from what is already saved."
            ),
            Resume::Unsupported => format!(
                "{stopped} while this turn was running, and it cannot be continued automatically because an earlier step did not finish. Its work so far is kept in the session; inspect it before sending new work."
            ),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    #[serde(rename = "messageID")]
    input: String,
    kind: String,
    by: String,
    resume: String,
}

fn decode(
    session: &SessionId,
    position: CommittedPosition,
    data: Data,
) -> Result<InterruptedTurn, String> {
    if data.session != session.as_str() || data.kind != INTERRUPTED_TURN {
        return Err("interrupted-turn notice identity is invalid".into());
    }
    let by = StoppedBy::from_word(&data.by)
        .ok_or("interrupted-turn notice names an unknown stop reason")?;
    // A reason this build does not know is a later build's continuation
    // policy: it is announced, and never guessed at.
    let resume = Resume::from_word(&data.resume).unwrap_or(Resume::Unsupported);
    Ok(InterruptedTurn {
        position,
        execution: ExecutionId::new(data.execution)
            .ok_or("interrupted-turn notice execution is invalid")?,
        input: data
            .input
            .parse()
            .map_err(|_| "interrupted-turn notice input is invalid")?,
        by,
        resume,
    })
}

/// Record the turn `execution` was running when `by` stopped the service.
///
/// The turn must be the Session's current input, and its execution must already
/// have an `interrupted` terminal: this records a fact the log already holds,
/// and cannot invent one. A repeated record for the same execution is an
/// idempotent read.
pub fn record_interrupted_turn(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    input: &MessageId,
    by: StoppedBy,
    resume: Resume,
) -> Result<InterruptedTurn, String> {
    let aggregate = AggregateId::Session(session.clone());
    // Consume the already elected writer; a notice cannot elect ownership.
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    SessionStore::new(&tx)
        .record(session)
        .map_err(|error| error.to_string())?;
    let interrupted = crate::execution::read(&tx, session, execution)?
        .get(1)
        .is_some_and(|held| {
            held.outcome == Some(crate::execution::Outcome::Interrupted)
                && held.interruption.is_some()
        });
    if !interrupted {
        return Err("interrupted-turn notice has no committed interruption".into());
    }
    let data = serde_json::to_value(Data {
        session: session.as_str().to_owned(),
        execution: execution.as_str().to_owned(),
        input: input.as_str().to_owned(),
        kind: INTERRUPTED_TURN.to_owned(),
        by: by.word().to_owned(),
        resume: resume.word().to_owned(),
    })
    .map_err(|error| error.to_string())?;
    if let Some(prior) = prior_for(&tx, session, execution)? {
        if prior.input != *input {
            return Err("interrupted-turn notice names a different turn".into());
        }
        tx.commit().map_err(|error| error.to_string())?.discard();
        return Ok(prior);
    }
    let position = append_in(&tx, aggregate, writer, authorities, &data)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(InterruptedTurn {
        position,
        execution: execution.clone(),
        input: input.clone(),
        by,
        resume,
    })
}

/// The newest recorded stop of `execution`, when one exists. Repeating the
/// record therefore returns the committed row instead of appending a second.
fn prior_for(
    connection: &rusqlite::Connection,
    session: &SessionId,
    execution: &ExecutionId,
) -> Result<Option<InterruptedTurn>, String> {
    let rows = matching_rows::<Data>(connection, session, INTERRUPTED_TURN, None, MAX_NOTICES)?;
    let mut found: Option<InterruptedTurn> = None;
    for (position, data) in rows {
        let decoded = decode(session, position, data)?;
        if decoded.execution != *execution {
            continue;
        }
        if found.is_some() {
            return Err("interrupted-turn notice is duplicated for one execution".into());
        }
        found = Some(decoded);
    }
    Ok(found)
}

/// The Session's newest recorded stops, at most [`MAX_NOTICES`], oldest first.
pub fn interrupted_turns(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Vec<InterruptedTurn>, String> {
    interrupted_turns_at(connection, session, None)
}

/// As [`interrupted_turns`], at most one row and only at `sequence` when it is
/// given: the exact-row read a paged transcript needs.
pub(crate) fn interrupted_turns_at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    sequence: Option<u64>,
) -> Result<Vec<InterruptedTurn>, String> {
    kind_rows_at::<Data>(connection, session, INTERRUPTED_TURN, sequence)?
        .into_iter()
        .map(|(position, data)| decode(session, position, data))
        .collect()
}

/// The Session's newest stop that a reader may still continue, when nothing has
/// happened since to make it stale.
///
/// The original execution's terminal is immutable, so rereading it proves only
/// that the turn *was* interrupted. What can change the answer is everything
/// after it: a later execution (a completed successor, or a new stop the user
/// chose), a later promoted input, or a user's own stop. Any of those means the
/// recorded turn is no longer the Session's live work, so the marker is not
/// offered again.
pub fn pending_resume(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<InterruptedTurn>, String> {
    let Some(newest) = interrupted_turns(connection, session)?.pop() else {
        return Ok(None);
    };
    if newest.resume != Resume::Resumable {
        return Ok(None);
    }
    let history = crate::execution::read(connection, session, &newest.execution)?;
    let Some(interrupted) = history.get(1) else {
        return Ok(None);
    };
    if interrupted.outcome != Some(crate::execution::Outcome::Interrupted) {
        return Ok(None);
    }
    // Our own stop must be the managed one. A later user stop on this same
    // execution would have kept `Superseded`/`User`, and either way the user
    // stopped the work: never continue it.
    if !matches!(
        interrupted.interruption,
        Some(crate::execution::InterruptionReason::Shutdown)
            | Some(crate::execution::InterruptionReason::ServeWindowExpired)
    ) {
        return Ok(None);
    }
    // Anything the Session did after this stop supersedes it.
    if superseded(connection, session, &newest)? {
        return Ok(None);
    }
    Ok(Some(newest))
}

/// Whether the Session recorded a later execution, a later promoted input, or a
/// later stop after `turn`.
fn superseded(
    connection: &rusqlite::Connection,
    session: &SessionId,
    turn: &InterruptedTurn,
) -> Result<bool, String> {
    let stopped_at = i64::try_from(turn.position.seq).map_err(|error| error.to_string())?;
    let later_execution: Option<i64> = connection
        .query_row(
            "SELECT seq FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type = 'session.execution.started.1' AND seq > ?2
             ORDER BY seq LIMIT 1",
            (session.as_str(), stopped_at),
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| {
            if error == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(error)
            }
        })
        .map_err(|error| error.to_string())?;
    if later_execution.is_some() {
        return Ok(true);
    }
    // A later promoted input is new work; this marker is not its turn. The
    // marker's own input was promoted before the stop, so the boundary is the
    // stop's own position, not the input's. A store whose projection tables are
    // absent has no promoted inputs at all, which cannot supersede anything.
    let later_input: Option<i64> = connection
        .query_row(
            "SELECT seq FROM session_message WHERE session_id = ?1 AND seq > ?2
             AND type IN ('user', 'synthetic') ORDER BY seq LIMIT 1",
            (session.as_str(), stopped_at),
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            rusqlite::Error::SqliteFailure(_, Some(ref detail))
                if detail == "no such table: session_message" =>
            {
                Ok(None)
            }
            other => Err(other),
        })
        .map_err(|error| error.to_string())?;
    Ok(later_input.is_some())
}
