//! Tolerant readers, W4: an execution failure row written by a LATER build —
//! a class name and message wording this build has never seen — is still
//! projected verbatim by the durable reader. The rows are staged through the
//! raw database seam, because no public path can write a name this build's
//! vocabulary does not hold: the writer's `Failure::public` holds exactly the
//! names it knows. Only structural damage is refused.

use grapt_session::{create_session, execution, SessionSeed};
use grapt_store::EventStore;
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, ExecutionId, IdAuthority, IdToken, Timestamp,
};
use std::cell::Cell;

#[derive(Debug, Default)]
struct Authorities(Cell<u64>);
impl ClockAuthority for Authorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1234)
    }
}
impl IdAuthority for Authorities {
    fn mint(&self) -> IdToken {
        self.0.set(self.0.get() + 1);
        IdToken::new(format!("futurevocab{}", self.0.get()))
            .unwrap_or_else(|| panic!("fixture ID refused"))
    }
}

#[test]
fn a_future_execution_failure_vocabulary_is_projected_verbatim(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "grapt-future-execution-vocab-{}.sqlite",
        std::process::id()
    ));
    assert!(
        !path.exists(),
        "refusing to overwrite existing witness data"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "future-execution-vocab-witness")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_future_vocab").ok_or("project ID refused")?,
        "/work/future-vocab",
        "future-vocab",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    drop(store);

    // The rows only a later build would write: its own start, and a failure
    // named with a class this build's vocabulary does not hold.
    let damaged = rusqlite::Connection::open(&path)?;
    let session = session.as_str().to_owned();
    let started = format!(r#"{{"sessionID":"{session}","executionID":"exe_future"}}"#);
    let failed = format!(
        r#"{{"sessionID":"{session}","executionID":"exe_future","error":{{"type":"ProviderQuotaExhausted","message":"the future build's own wording for this failure"}}}}"#
    );
    damaged.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
         VALUES (?1, 'session', ?2, 100, 1000, 'session.execution.started.1', ?3, 1)",
        rusqlite::params!["evt_future_start", session, started],
    )?;
    damaged.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
         VALUES (?1, 'session', ?2, 101, 2000, 'session.execution.failed.1', ?3, 1)",
        rusqlite::params!["evt_future_failed", session, failed],
    )?;

    // The durable reader projects the future row verbatim: the stored name
    // and message are what a transcript reader sees, never a refusal and
    // never this build's wording in their place.
    let session_id: grapt_types::SessionId = session
        .parse()
        .map_err(|error| format!("the fixture session identity parses: {error}"))?;
    let observations = execution::failures(&damaged, &session_id)?;
    let only = observations
        .into_iter()
        .next()
        .ok_or("the future failure row must be projected")?;
    let failure = only
        .failure
        .as_ref()
        .ok_or("the projected row lost its failure")?;
    assert_eq!(failure.name, "ProviderQuotaExhausted");
    assert_eq!(
        failure.message,
        "the future build's own wording for this failure"
    );
    assert!(
        !failure.is_known_name(),
        "the stored name is untrusted, so the display layer must say so"
    );
    let execution_id = ExecutionId::new("exe_future").ok_or("execution ID refused")?;
    let lifecycle = execution::read(&damaged, &session_id, &execution_id)?;
    assert_eq!(lifecycle.len(), 2, "start and failure form one lifecycle");
    assert_eq!(lifecycle[1].outcome, Some(execution::Outcome::Failed));

    // Structural damage is still corruption: a blank message is not a row.
    damaged.execute(
        "UPDATE event SET data = json_set(data, '$.error.message', '')
         WHERE type = 'session.execution.failed.1'",
        [],
    )?;
    assert!(
        execution::failures(&damaged, &session_id).is_err(),
        "a blank message is corruption, not future vocabulary"
    );
    drop(damaged);
    std::fs::remove_file(path)?;
    Ok(())
}
