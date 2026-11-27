//! Restart recovery records every still-active Session as disconnected,
//! however many pages they span, and a recovered Session is no longer active.
use grapt_service_state::{record, recover, Change, Kind, State};
use grapt_session::{create_session, SessionSeed};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{
    ClockAuthority, DeterministicAuthorities, ExecutionId, IdAuthority, IdToken, SessionId,
    Timestamp,
};
use std::cell::Cell;

#[derive(Debug, Default)]
struct Authorities(Cell<u64>);

impl ClockAuthority for Authorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_786_060_800_000)
    }
}

impl IdAuthority for Authorities {
    fn mint(&self) -> IdToken {
        let next = self.0.get() + 1;
        self.0.set(next);
        match IdToken::new(format!("recovery{next:08}")) {
            Some(token) => token,
            None => panic!("a counted identity is never empty"),
        }
    }
}

#[test]
fn every_active_session_is_recovered_across_pages() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let authorities = Authorities::default();
    let both = DeterministicAuthorities::new(&authorities, &authorities);
    let directory = std::env::temp_dir();
    let directory = directory
        .to_str()
        .ok_or("temporary directory is not UTF-8")?;
    for index in 0..10 {
        let session: SessionId = format!("ses_recovery_{index:02}").parse()?;
        let project = grapt_types::ProjectId::new("prj_recovery").ok_or("project")?;
        let seed =
            SessionSeed::new(project, directory, "recovery")?.with_requested_id(session.clone())?;
        create_session(&mut store, &both, &seed, &mut [])?;
        store.fencing_token(&AggregateId::Session(session.clone()))?;
        let execution = ExecutionId::new(format!("exe_recovery_{index:02}")).ok_or("execution")?;
        let running = State::plain(Kind::Running);
        record(
            &mut store,
            &both,
            &session,
            &execution,
            running,
            Change::Start,
        )?;
    }
    let recovery = recover(&mut store, &both, 3)?;
    assert_eq!(recovery.sessions.len(), 10, "{recovery:?}");
    assert!(recovery.warnings.is_empty(), "{recovery:?}");
    let again = recover(&mut store, &both, 3)?;
    assert!(
        again.sessions.is_empty(),
        "a recovered Session is active again: {again:?}"
    );
    Ok(())
}
