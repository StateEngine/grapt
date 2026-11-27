//! A persisted active observation cannot resurrect its process after restart.
//!
//! Every Session whose status still says running or waiting is recorded as
//! disconnected by process loss. Sessions are read in pages of `page` by
//! identity, so a restart after any number of active Sessions settles all of
//! them in memory bounded by one page. A Session that cannot be recorded is
//! skipped with a warning and the pass moves past it. A status row that is not
//! JSON never stops the listing; its Session is judged by its other rows.
//!
//! An active status is the durable mark that a crash interrupted its Session,
//! so the caller's per-Session work runs before it is recorded disconnected;
//! if that work fails, the Session stays active for the next start to find.
use super::*;
use grapt_store::EventStore;
use grapt_types::{DeterministicAuthorities, SessionId};

/// Warnings kept in full; later ones are only counted.
pub const MAX_RECOVERY_WARNINGS: usize = 64;

/// What one restart recovery did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Recovery {
    /// Sessions recorded as disconnected.
    pub sessions: usize,
    /// Sessions skipped, each with its reason, up to [`MAX_RECOVERY_WARNINGS`].
    pub warnings: Vec<String>,
    /// Warnings past the kept ones.
    pub warnings_omitted: usize,
}

impl Recovery {
    fn warn(&mut self, warning: String) {
        if self.warnings.len() < MAX_RECOVERY_WARNINGS {
            self.warnings.push(warning);
        } else {
            self.warnings_omitted = self.warnings_omitted.saturating_add(1);
        }
    }
}

/// Record every still-active Session as disconnected, each after `first`
/// succeeds for it holding its writer. Fails only when the active Sessions
/// cannot be listed at all.
pub fn recover(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    page: usize,
    first: &mut dyn FnMut(&mut EventStore, &SessionId) -> Result<(), String>,
) -> Result<Recovery, String> {
    let page = i64::try_from(page.max(1)).map_err(|error| error.to_string())?;
    let mut recovery = Recovery::default();
    let mut after = String::new();
    loop {
        let sessions = active_after(store, &after, page)?;
        let Some(last) = sessions.last() else {
            return Ok(recovery);
        };
        after = last.clone();
        for raw in sessions {
            match recover_one(store, authorities, &raw, first) {
                Ok(()) => recovery.sessions = recovery.sessions.saturating_add(1),
                Err(reason) => recovery.warn(format!("session {raw}: {reason}")),
            }
        }
    }
}

/// One page of active Session identities after `after`, in identity order.
fn active_after(store: &EventStore, after: &str, page: i64) -> Result<Vec<String>, String> {
    let mut statement = store
        .connection()
        .prepare(
            "SELECT DISTINCT e.aggregate_id FROM event e
             WHERE e.aggregate_family = 'session' AND e.aggregate_id > ?2
             AND (e.type = 'session.execution.started.1' OR
                  (e.type IN ('session.status.started.1', 'session.status.changed.1') AND
                   CASE WHEN json_valid(e.data) THEN json_extract(e.data, '$.state.type') END
                   IN ('running', 'waiting')))
             AND NOT EXISTS (SELECT 1 FROM event newer
               WHERE newer.aggregate_family = 'session' AND newer.aggregate_id = e.aggregate_id
               AND newer.seq > e.seq AND
               (newer.type IN ('session.status.started.1', 'session.execution.started.1') OR
                (newer.type IN ('session.status.changed.1', 'session.execution.succeeded.1',
                 'session.execution.failed.1', 'session.execution.interrupted.1') AND
                 CASE WHEN json_valid(newer.data) AND json_valid(e.data)
                   THEN json_extract(newer.data, '$.executionID') = json_extract(e.data, '$.executionID')
                 END)))
             ORDER BY e.aggregate_id LIMIT ?1",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map(rusqlite::params![page, after], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| error.to_string())?;
    rows.collect::<Result<_, _>>()
        .map_err(|error| error.to_string())
}

fn recover_one(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    raw: &str,
    first: &mut dyn FnMut(&mut EventStore, &SessionId) -> Result<(), String>,
) -> Result<(), String> {
    let session = raw
        .parse::<SessionId>()
        .map_err(|error| error.to_string())?;
    let snapshot = read(store.connection(), &session)?;
    let execution = snapshot
        .execution
        .ok_or("active session status has no execution")?;
    let aggregate = grapt_store::AggregateId::Session(session.clone());
    match store.fencing_token(&aggregate) {
        Ok(_) => {}
        Err(grapt_store::WriteError::NotElected { .. }) => {
            store
                .elect_writer(&aggregate)
                .map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    first(store, &session).map_err(|reason| format!("kept active for the next start: {reason}"))?;
    record(store, authorities, &session, &execution,
        State::described(Kind::Disconnected, Reason::ProcessLoss,
            "The service restarted before this execution completed; work so far is kept. Send a follow-up prompt to continue.", Some(Action::FollowUp)),
        Change::Recover)?;
    Ok(())
}
