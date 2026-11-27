//! A delayed owner observation cannot replace newer committed detail on the stream.
use super::*;
use crate::dogfood::session_status::SessionStatusEmitter;
use grapt_api::u3a::{ServerConnectedEvent, SessionStatusInfo, SseLimitValues, SseLimits};
use grapt_service_state::{Action, Change, Reason, State};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn wait_state(reason: Reason) -> State {
    State::described(
        grapt_service_state::Kind::Waiting,
        reason,
        "Waiting for the current input request.",
        Some(Action::Wait),
    )
}

struct Fixture {
    session: SessionId,
    emitter: Arc<SessionStatusEmitter>,
    active: Arc<LiveActiveExecutions>,
    hub: Arc<DogfoodEventHub>,
}

fn fixture() -> Result<Fixture> {
    let ids = Arc::new(OsRandomIds::seed()?);
    let clock = Arc::new(SystemClock);
    let limits = SseLimits::try_new(SseLimitValues {
        subscribers: 1,
        pending_events: 8,
        event_bytes: grapt_api::u3a::MAX_SSE_EVENT_BYTES,
        frame_bytes: grapt_api::u3a::MAX_SSE_FRAME_BYTES,
    })?;
    let hub = Arc::new(DogfoodEventHub::try_new(
        limits,
        ServerConnectedEvent::new(WireId::parse("evt_revision_fixture".into()).ok_or("event")?),
    )?);
    let inbox = Arc::new(Mutex::new(Inbox::new(
        grapt_store::EventStore::open_in_memory()?,
    )?));
    let authorities = DeterministicAuthorities::new(&*clock, &*ids);
    let directory = std::env::temp_dir();
    let directory = directory
        .to_str()
        .ok_or("fixture temporary directory is not UTF-8")?;
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_revision").ok_or("project")?,
        directory,
        "revision fixture",
    )?;
    let session = SessionPorts::new(Arc::clone(&inbox), &authorities)
        .create(&seed)?
        .session;
    let emitter = SessionStatusEmitter::journaled(&ids, &clock, &hub, &inbox);
    let active = Arc::new(LiveActiveExecutions::with_status(Arc::clone(&emitter)));
    Ok(Fixture {
        session,
        emitter,
        active,
        hub,
    })
}

fn states(
    subscription: &grapt_api::u3a::DogfoodEventSubscription,
) -> Result<Vec<serde_json::Value>> {
    subscription.cancel();
    let mut states = Vec::new();
    while let Some(frame) = subscription.next_frame()? {
        let text = std::str::from_utf8(&frame)?;
        if let Some(data) = text.lines().find_map(|line| line.strip_prefix("data: ")) {
            let event: serde_json::Value = serde_json::from_str(data)?;
            if let Some(state) = event.get("data").and_then(|data| data.get("state")) {
                states.push(state.clone());
            }
        }
    }
    Ok(states)
}

#[test]
fn a_delayed_status_offer_cannot_regress_same_owner_committed_revision() -> Result {
    let Fixture {
        session,
        emitter,
        active,
        hub,
    } = fixture()?;
    let execution = ExecutionId::new("exe_revision_fixture").ok_or("execution")?;
    let owner = active.register(session.clone(), execution.clone(), 1)?;
    let subscription = hub.subscribe_host()?;
    let (committed, committed_rx) = std::sync::mpsc::sync_channel(1);
    let (release, release_rx) = std::sync::mpsc::sync_channel(1);
    let worker = {
        let (emitter, active, session, execution) = (
            Arc::clone(&emitter),
            Arc::clone(&active),
            session.clone(),
            execution.clone(),
        );
        std::thread::spawn(move || {
            let first = AtomicBool::new(true);
            emitter.observe_owned(
                &session,
                &execution,
                Change::Start,
                |_| Ok(Some(wait_state(Reason::Permission))),
                |_| {},
                |snapshot| {
                    if first.swap(false, Ordering::SeqCst) {
                        committed.send(()).map_err(|error| error.to_string())?;
                        release_rx
                            .recv_timeout(Duration::from_secs(5))
                            .map_err(|error| error.to_string())?;
                    }
                    active
                        .status_presentation(&session, snapshot)
                        .map_err(|_| "owner confirmation failed".to_owned())
                },
            )
        })
    };
    committed_rx.recv_timeout(Duration::from_secs(5))?;
    emitter.observe_owned(
        &session,
        &execution,
        Change::Update,
        |_| Ok(Some(wait_state(Reason::Question))),
        |_| {},
        |snapshot| {
            let presentation = active
                .status_presentation(&session, snapshot)
                .map_err(|_| "owner confirmation failed".to_owned())?;
            assert_eq!(presentation.status, SessionStatusInfo::busy());
            Ok(presentation)
        },
    )?;
    release.send(())?;
    let _delayed_offer = worker
        .join()
        .map_err(|_| "delayed status producer panicked")?;
    let states = states(&subscription)?;
    drop(owner);
    assert!(
        !states.is_empty(),
        "no authoritative status reached the subscriber"
    );
    assert!(
        states
            .windows(2)
            .all(|pair| pair[0]["revision"].as_u64() <= pair[1]["revision"].as_u64()),
        "committed status revision regressed: {states:?}"
    );
    assert_eq!(
        states.last().and_then(|state| state["reason"].as_str()),
        Some("question"),
        "delayed permission status replaced the newer question state"
    );
    Ok(())
}
