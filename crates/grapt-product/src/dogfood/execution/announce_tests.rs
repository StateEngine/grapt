//! Witnesses for the terminal announcement order: the
//! durable terminal, then out of the active registry, then published.

use super::observations::announce;
use super::*;

/// A Session with one begun execution, in a fresh in-memory store.
fn begun() -> Result<(grapt_store::EventStore, SessionId, ExecutionId), String> {
    let mut store = grapt_store::EventStore::open_in_memory().map_err(|error| error.to_string())?;
    let ids = OsRandomIds::seed().map_err(|error| error.to_string())?;
    let clock = SystemClock;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_announce").ok_or("project id")?,
        "/work/announce",
        "announce",
    )
    .map_err(|error| error.to_string())?;
    let session = grapt_session::create_session(&mut store, &authorities, &seed, &mut [])
        .map_err(|error| error.to_string())?
        .session;
    let execution = ExecutionId::new("exe_announce").ok_or("execution id")?;
    grapt_session::execution::begin(&mut store, &authorities, &session, &execution)?;
    Ok((store, session, execution))
}

fn settle(
    store: &mut grapt_store::EventStore,
    session: &SessionId,
    execution: &ExecutionId,
    outcome: grapt_session::execution::Outcome,
) -> Result<grapt_session::execution::Observation, String> {
    let ids = OsRandomIds::seed().map_err(|error| error.to_string())?;
    let clock = SystemClock;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    grapt_session::execution::settle(store, &authorities, session, execution, outcome)
}

#[test]
fn a_published_terminal_is_never_followed_by_an_active_execution() -> Result<(), String> {
    let (mut store, session, execution) = begun()?;
    let durable = settle(
        &mut store,
        &session,
        &execution,
        grapt_session::execution::Outcome::Succeeded,
    );
    let active = LiveActiveExecutions::default();
    let _guard = active.register(session.clone(), execution.clone(), 4)?;
    let hub = super::super::event_hub()?;
    let subscriber = hub.subscribe_host().map_err(|error| error.to_string())?;
    // The connected frame precedes every event.
    let _connected = subscriber.next_frame().map_err(|error| error.to_string())?;
    announce(&active, &hub, &session, &execution, durable).map_err(|error| error.to_string())?;
    let frame = subscriber
        .next_frame()
        .map_err(|error| error.to_string())?
        .ok_or("normal terminal ended its subscription")?;
    assert!(String::from_utf8_lossy(&frame).contains("session.execution.succeeded"));
    assert!(
        active.projected(vec![session.clone()]).is_empty(),
        "a published terminal was followed by an active execution"
    );
    // A second committed execution reaches the SAME subscription.
    let next = ExecutionId::new("exe_announce_next").ok_or("next execution id")?;
    let ids = OsRandomIds::seed().map_err(|error| error.to_string())?;
    let clock = SystemClock;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let observation = grapt_session::execution::begin(&mut store, &authorities, &session, &next)?;
    announce(&active, &hub, &session, &next, Ok(observation)).map_err(|error| error.to_string())?;
    let frame = subscriber
        .next_frame()
        .map_err(|error| error.to_string())?
        .ok_or("normal terminal closed the continuing stream")?;
    assert!(String::from_utf8_lossy(&frame).contains("session.execution.started"));
    Ok(())
}

/// A terminal whose durable append failed settles
/// nothing, so the registry never claims idle ahead of a durable failure.
#[test]
fn a_terminal_that_failed_to_append_leaves_the_execution_active() -> Result<(), String> {
    let (mut store, session, execution) = begun()?;
    settle(
        &mut store,
        &session,
        &execution,
        grapt_session::execution::Outcome::Succeeded,
    )?;
    // A different terminal for the settled execution: the store refuses to
    // resurrect its lifecycle, so this terminal's append fails.
    let refused = settle(
        &mut store,
        &session,
        &execution,
        grapt_session::execution::Outcome::Interrupted,
    );
    assert!(
        refused.is_err(),
        "a second, different terminal was appended"
    );
    let active = LiveActiveExecutions::default();
    let _guard = active.register(session.clone(), execution.clone(), 4)?;
    let hub = super::super::event_hub()?;
    assert!(announce(&active, &hub, &session, &execution, refused).is_err());
    assert_eq!(
        active.projected(vec![session.clone()]),
        vec![(session, execution)],
        "a failed durable terminal settled the execution"
    );
    Ok(())
}

/// The terminal commits even when its live frame exceeds the bound. The
/// publication receipt must retain invalidation and permit a bounded rebind.
#[test]
fn a_refused_terminal_publication_invalidates_and_rebinds_within_the_bound() -> Result<(), String> {
    use super::observations::ObservationPublication;
    use grapt_api::u3a::{ServerConnectedEvent, SseLimitValues, SseLimits};
    let (mut store, session, execution) = begun()?;
    let durable = settle(
        &mut store,
        &session,
        &execution,
        grapt_session::execution::Outcome::Succeeded,
    );
    let active = LiveActiveExecutions::default();
    let _guard = active.register(session.clone(), execution.clone(), 4)?;
    let limits = SseLimits::try_new(SseLimitValues {
        subscribers: 1,
        pending_events: 2,
        event_bytes: 128,
        frame_bytes: 512,
    })
    .map_err(|error| error.to_string())?;
    let connected = WireId::parse("evt_bounded".to_owned()).ok_or("connected id")?;
    let hub = DogfoodEventHub::try_new(limits, ServerConnectedEvent::new(connected))
        .map_err(|error| error.to_string())?;
    let old = hub.subscribe_host().map_err(|error| error.to_string())?;
    let receipt = announce(&active, &hub, &session, &execution, durable)
        .map_err(|error| error.to_string())?;
    assert_eq!(receipt, ObservationPublication::Unavailable);
    assert!(active.projected(vec![session.clone()]).is_empty());
    assert!(
        old.next_frame().is_err(),
        "refused publication left queued frames valid"
    );
    assert!(
        settle(
            &mut store,
            &session,
            &execution,
            grapt_session::execution::Outcome::Interrupted
        )
        .is_err(),
        "refused publication lost its already committed terminal"
    );
    let rebound = hub.subscribe_host().map_err(|error| error.to_string())?;
    let frame = rebound
        .next_frame()
        .map_err(|error| error.to_string())?
        .ok_or("bounded rebind never connected")?;
    assert!(String::from_utf8_lossy(&frame).contains("server.connected"));
    Ok(())
}
