//! A persisted active observation cannot resurrect its process after restart.
use super::*;
use grapt_store::EventStore;
use grapt_types::{DeterministicAuthorities, SessionId};

pub fn recover(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    limit: usize,
) -> Result<usize, String> {
    let sessions = {
        let mut statement = store.connection().prepare(
            "SELECT e.aggregate_id FROM event e
             WHERE e.aggregate_family = 'session'
             AND (e.type = 'session.execution.started.1' OR
                  (e.type IN ('session.status.started.1', 'session.status.changed.1') AND
                   json_extract(e.data, '$.state.type') IN ('running', 'waiting')))
             AND NOT EXISTS (SELECT 1 FROM event newer
               WHERE newer.aggregate_family = 'session' AND newer.aggregate_id = e.aggregate_id
               AND newer.seq > e.seq AND
               (newer.type IN ('session.status.started.1', 'session.execution.started.1') OR
                (newer.type IN ('session.status.changed.1', 'session.execution.succeeded.1',
                 'session.execution.failed.1', 'session.execution.interrupted.1') AND
                 json_extract(newer.data, '$.executionID') = json_extract(e.data, '$.executionID')))) LIMIT ?1"
        ).map_err(|error| error.to_string())?;
        let bound = i64::try_from(
            limit
                .checked_add(1)
                .ok_or("session recovery bound overflow")?,
        )
        .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([bound], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(
                row.map_err(|error| error.to_string())?
                    .parse::<SessionId>()
                    .map_err(|error| error.to_string())?,
            );
        }
        if sessions.len() > limit {
            return Err("unsettled session status exceeds its recovery bound".into());
        }
        sessions
    };
    for session in &sessions {
        let snapshot = read(store.connection(), session)?;
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
        record(store, authorities, session, &execution,
            State::described(Kind::Disconnected, Reason::ProcessLoss,
                "The service restarted before this execution completed; work so far is kept. Send a follow-up prompt to continue.", Some(Action::FollowUp)),
            Change::Recover)?;
    }
    Ok(sessions.len())
}
