//! The restart record is exactly what a later reader may continue, and a user's
//! own stop is never one.
use super::*;
use crate::{create_session, SessionSeed};
use grapt_store::EventStore;
use grapt_types::{ClockAuthority, IdAuthority, IdToken, ProjectId, Timestamp};
use grapt_types::{DeterministicAuthorities, ExecutionId, MessageId, SessionId};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
struct FrozenClock(i64);

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

#[derive(Debug, Default)]
struct CountingIds(AtomicU64);

impl IdAuthority for CountingIds {
    fn mint(&self) -> IdToken {
        let next = self.0.fetch_add(1, Ordering::SeqCst);
        match IdToken::new(format!("tst{next:012x}")) {
            Some(token) => token,
            None => std::process::abort(),
        }
    }
}

fn seed() -> Result<SessionSeed, String> {
    let project = ProjectId::new("project".to_owned()).ok_or("project identity is empty")?;
    SessionSeed::new(project, "/work/session", "Interrupted turn")
        .map_err(|error| error.to_string())
}

fn interrupted(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
) -> Result<(), String> {
    crate::execution::begin(store, authorities, session, execution).map(|_| ())?;
    crate::execution::Change::Interrupted(crate::execution::InterruptionReason::Shutdown)
        .record(store, authorities, session, execution)
        .map(|_| ())
}

#[test]
fn a_service_stop_is_pending_resume_and_a_uniquely_recorded_turn_is_not_duplicated(
) -> Result<(), String> {
    let mut store = EventStore::open_in_memory().map_err(|error| error.to_string())?;
    let clock = FrozenClock(42);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = crate::SessionListProjector;
    let session = create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])
        .map_err(|error| error.to_string())?
        .session;
    let execution: ExecutionId = ExecutionId::new("exe_service_stop").ok_or("execution id")?;
    let input: MessageId = "msg_interrupted_turn".parse().map_err(|_| "message id")?;
    interrupted(&mut store, &authorities, &session, &execution)?;

    let recorded = record_interrupted_turn(
        &mut store,
        &authorities,
        &session,
        &execution,
        &input,
        StoppedBy::ServiceShutdown,
        Resume::Resumable,
    )?;
    assert_eq!(recorded.by, StoppedBy::ServiceShutdown);
    assert_eq!(recorded.input, input);

    // Repeating the record is an idempotent read of the committed row.
    let again = record_interrupted_turn(
        &mut store,
        &authorities,
        &session,
        &execution,
        &input,
        StoppedBy::ServiceShutdown,
        Resume::Resumable,
    )?;
    assert_eq!(again.position.seq, recorded.position.seq);

    let pending = pending_resume(store.connection(), &session)?
        .ok_or("a resumable service stop was not offered for continuation")?;
    assert_eq!(pending.execution, execution);
    assert_eq!(pending.input, input);

    // The reader gets exactly one line, and it names the action that works.
    let turns = interrupted_turns(store.connection(), &session)?;
    assert_eq!(turns.len(), 1);
    assert!(turns[0].text().contains("Send a follow-up to continue"));
    Ok(())
}

#[test]
fn an_unsupported_turn_is_announced_but_never_offered_for_continuation() -> Result<(), String> {
    let mut store = EventStore::open_in_memory().map_err(|error| error.to_string())?;
    let clock = FrozenClock(7);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = crate::SessionListProjector;
    let session = create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])
        .map_err(|error| error.to_string())?
        .session;
    let execution: ExecutionId = ExecutionId::new("exe_unsupported").ok_or("execution id")?;
    let input: MessageId = "msg_unsupported".parse().map_err(|_| "message id")?;
    interrupted(&mut store, &authorities, &session, &execution)?;
    record_interrupted_turn(
        &mut store,
        &authorities,
        &session,
        &execution,
        &input,
        StoppedBy::ServiceShutdown,
        Resume::Unsupported,
    )?;

    assert_eq!(pending_resume(store.connection(), &session)?, None);
    let turns = interrupted_turns(store.connection(), &session)?;
    assert_eq!(turns.len(), 1);
    assert!(turns[0]
        .text()
        .contains("cannot be continued automatically"));
    Ok(())
}

#[test]
fn a_record_without_a_committed_interruption_is_refused() -> Result<(), String> {
    let mut store = EventStore::open_in_memory().map_err(|error| error.to_string())?;
    let clock = FrozenClock(11);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = crate::SessionListProjector;
    let session = create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])
        .map_err(|error| error.to_string())?
        .session;
    let execution: ExecutionId = ExecutionId::new("exe_never_started").ok_or("execution id")?;
    let input: MessageId = "msg_never_started".parse().map_err(|_| "message id")?;
    let refused = record_interrupted_turn(
        &mut store,
        &authorities,
        &session,
        &execution,
        &input,
        StoppedBy::ServiceShutdown,
        Resume::Resumable,
    );
    assert!(
        refused.is_err_and(|error| error.contains("no committed interruption")),
        "a record was accepted without the interruption it describes"
    );
    Ok(())
}

#[test]
fn a_completed_successor_makes_the_stop_stale() -> Result<(), String> {
    let mut store = EventStore::open_in_memory().map_err(|error| error.to_string())?;
    let clock = FrozenClock(31);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = crate::SessionListProjector;
    let session = create_session(&mut store, &authorities, &seed()?, &mut [&mut projector])
        .map_err(|error| error.to_string())?
        .session;
    let stopped: ExecutionId = ExecutionId::new("exe_stopped").ok_or("execution id")?;
    let input: MessageId = "msg_stopped_turn".parse().map_err(|_| "message id")?;
    interrupted(&mut store, &authorities, &session, &stopped)?;
    record_interrupted_turn(
        &mut store,
        &authorities,
        &session,
        &stopped,
        &input,
        StoppedBy::ServiceShutdown,
        Resume::Resumable,
    )?;
    assert!(
        pending_resume(store.connection(), &session)?.is_some(),
        "the stop was not offered before anything superseded it"
    );

    // A successor execution begins after the stop: the recorded turn is no
    // longer the Session's live work, so it must not be offered again.
    let successor: ExecutionId = ExecutionId::new("exe_successor").ok_or("execution id")?;
    crate::execution::begin(&mut store, &authorities, &session, &successor)
        .map_err(|error| error.to_string())?;
    assert_eq!(
        pending_resume(store.connection(), &session)?,
        None,
        "a later execution left the old stop eligible"
    );
    Ok(())
}
