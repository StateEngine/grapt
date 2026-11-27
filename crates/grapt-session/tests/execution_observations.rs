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
        IdToken::new(format!("executionobservation{}", self.0.get()))
            .unwrap_or_else(|| panic!("fixture ID refused"))
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn failure_before_any_step_is_durable_and_cannot_resurrect_as_success(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "grapt-execution-observation-{}.sqlite",
        std::process::id()
    ));
    assert!(
        !path.exists(),
        "refusing to overwrite existing witness data"
    );
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "execution-observation-witness")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_observation").ok_or("project ID refused")?,
        "/work/observation",
        "observation",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    let execution = ExecutionId::new("exe_observation").ok_or("execution ID refused")?;
    assert!(execution::failures(store.connection(), &session)?.is_empty());
    assert!(
        execution::settle(
            &mut store,
            &authorities,
            &session,
            &execution,
            execution::Outcome::Failed
        )
        .is_err(),
        "a missing start became a fabricated lifecycle"
    );
    let start = execution::begin(&mut store, &authorities, &session, &execution)?;
    assert!(
        execution::begin(&mut store, &authorities, &session, &execution).is_err(),
        "execution identity was reused"
    );
    let failed = execution::settle(
        &mut store,
        &authorities,
        &session,
        &execution,
        execution::Outcome::Failed,
    )?;
    assert!(failed.position.seq > start.position.seq);
    assert_eq!(
        execution::failures(store.connection(), &session)?,
        vec![failed.clone()]
    );
    let step_count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type LIKE 'session.step.%'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(step_count, 0, "pre-Step failure invented a Step terminal");
    drop(store);
    let mut store = EventStore::open(&path, "execution-observation-witness")?;
    assert_eq!(
        execution::failures(store.connection(), &session)?,
        vec![failed.clone()]
    );
    assert_eq!(
        execution::read(store.connection(), &session, &execution)?,
        vec![start, failed.clone()]
    );
    assert!(
        execution::settle(
            &mut store,
            &authorities,
            &session,
            &execution,
            execution::Outcome::Failed
        )
        .is_err(),
        "reopening an observation granted writer custody"
    );
    store.elect_writer(&grapt_store::AggregateId::Session(session.clone()))?;
    assert_eq!(
        execution::settle(
            &mut store,
            &authorities,
            &session,
            &execution,
            execution::Outcome::Failed
        )?,
        failed
    );
    assert!(
        execution::settle(
            &mut store,
            &authorities,
            &session,
            &execution,
            execution::Outcome::Succeeded
        )
        .is_err(),
        "failed owner resurrected as successful"
    );
    assert!(
        execution::begin(&mut store, &authorities, &session, &execution).is_err(),
        "restart re-admitted an old execution identity"
    );
    // A corrupt historical lifecycle is an error, never an empty failure list.
    drop(store);
    // Fault injection opens only this test's stopped, freshly created store.
    let damaged = rusqlite::Connection::open(&path)?;
    let original: String = damaged.query_row(
        "SELECT data FROM event WHERE type = 'session.execution.failed.1'",
        [],
        |row| row.get(0),
    )?;
    damaged.execute(
        "UPDATE event SET data = 'not-json' WHERE type = 'session.execution.failed.1'",
        [],
    )?;
    assert!(execution::failures(&damaged, &session).is_err());
    damaged.execute(
        "UPDATE event SET data = ?1 WHERE type = 'session.execution.failed.1'",
        [original],
    )?;
    drop(damaged);
    let mut store = EventStore::open(&path, "execution-observation-witness")?;
    store.elect_writer(&grapt_store::AggregateId::Session(session.clone()))?;
    for ordinal in 0..127 {
        let execution =
            ExecutionId::new(format!("exe_bound{ordinal}")).ok_or("bound fixture id refused")?;
        execution::begin(&mut store, &authorities, &session, &execution)?;
        execution::settle(
            &mut store,
            &authorities,
            &session,
            &execution,
            execution::Outcome::Failed,
        )?;
    }
    assert_eq!(
        execution::failures(store.connection(), &session)?.len(),
        128
    );
    let overflow = ExecutionId::new("exe_overflow").ok_or("overflow id refused")?;
    execution::begin(&mut store, &authorities, &session, &overflow)?;
    execution::settle(
        &mut store,
        &authorities,
        &session,
        &overflow,
        execution::Outcome::Failed,
    )?;
    let refusal = execution::failures(store.connection(), &session)
        .err()
        .ok_or("failure history silently exceeded its bound")?;
    assert!(refusal.contains("exceeds its bound"));
    drop(store);
    std::fs::remove_file(path)?;
    Ok(())
}
