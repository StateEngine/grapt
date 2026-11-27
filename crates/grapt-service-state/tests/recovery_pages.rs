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

/// Ten Sessions recorded as running in `store`.
fn running(
    store: &mut EventStore,
    authorities: &Authorities,
) -> Result<(), Box<dyn std::error::Error>> {
    let both = DeterministicAuthorities::new(authorities, authorities);
    let directory = std::env::temp_dir();
    let directory = directory
        .to_str()
        .ok_or("temporary directory is not UTF-8")?;
    for index in 0..10 {
        let session = session(index)?;
        let project = grapt_types::ProjectId::new("prj_recovery").ok_or("project")?;
        let seed =
            SessionSeed::new(project, directory, "recovery")?.with_requested_id(session.clone())?;
        create_session(store, &both, &seed, &mut [])?;
        store.fencing_token(&AggregateId::Session(session.clone()))?;
        let execution = ExecutionId::new(format!("exe_recovery_{index:02}")).ok_or("execution")?;
        let running = State::plain(Kind::Running);
        record(store, &both, &session, &execution, running, Change::Start)?;
    }
    Ok(())
}

fn session(index: usize) -> Result<SessionId, Box<dyn std::error::Error>> {
    Ok(format!("ses_recovery_{index:02}").parse()?)
}

fn nothing(_: &mut EventStore, _: &SessionId) -> Result<(), String> {
    Ok(())
}

#[test]
fn every_active_session_is_recovered_across_pages() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let authorities = Authorities::default();
    running(&mut store, &authorities)?;
    let both = DeterministicAuthorities::new(&authorities, &authorities);
    let recovery = recover(&mut store, &both, 3, &mut nothing)?;
    assert_eq!(recovery.sessions, 10, "{recovery:?}");
    assert!(recovery.warnings.is_empty(), "{recovery:?}");
    let again = recover(&mut store, &both, 3, &mut nothing)?;
    assert_eq!(
        again.sessions, 0,
        "a recovered Session is active again: {again:?}"
    );
    Ok(())
}

/// A Session whose work before recovery fails stays active, so the next start
/// still finds it, while every other Session is recovered.
#[test]
fn a_session_whose_first_step_fails_stays_active_for_the_next_start(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let authorities = Authorities::default();
    running(&mut store, &authorities)?;
    let both = DeterministicAuthorities::new(&authorities, &authorities);
    let failing = session(4)?;
    let mut first = |_: &mut EventStore, session: &SessionId| match *session == failing {
        true => Err("lost runs could not be settled".to_owned()),
        false => Ok(()),
    };
    let recovery = recover(&mut store, &both, 3, &mut first)?;
    assert_eq!(
        (recovery.sessions, recovery.warnings.len()),
        (9, 1),
        "{recovery:?}"
    );
    let again = recover(&mut store, &both, 3, &mut nothing)?;
    assert_eq!(
        again.sessions, 1,
        "the failed Session was not found again: {again:?}"
    );
    Ok(())
}

/// A status row that is not JSON costs at most its own Session; the others are
/// still listed and recovered.
#[test]
fn a_malformed_status_row_does_not_stop_the_others() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::TempDir::new()?;
    let path = dir.path().join("grapt-status-recovery-malformed.sqlite");
    let mut store = EventStore::open(&path, "status-recovery-malformed")?;
    let authorities = Authorities::default();
    running(&mut store, &authorities)?;
    let broken = session(2)?;
    let raw = rusqlite::Connection::open(&path)?;
    raw.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
         SELECT 'evt_recovery_malformed', aggregate_family, aggregate_id, seq + 1, created,
                'session.status.changed.1', '{not json', writer_epoch
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
         ORDER BY seq DESC LIMIT 1",
        [broken.as_str()],
    )?;
    raw.execute(
        "UPDATE event_sequence SET seq = seq + 1
         WHERE aggregate_family = 'session' AND aggregate_id = ?1",
        [broken.as_str()],
    )?;
    drop(raw);
    let both = DeterministicAuthorities::new(&authorities, &authorities);
    let recovery = recover(&mut store, &both, 3, &mut nothing)?;
    assert!(recovery.sessions >= 9, "{recovery:?}");
    assert_eq!(
        recovery.sessions + recovery.warnings.len(),
        10,
        "{recovery:?}"
    );
    drop(store);
    Ok(())
}
