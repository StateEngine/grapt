use super::config::from_documents;
use super::publisher::{Body, Command, Desired, Publisher, SeatHost, SeatSession, SeatTurn};
use super::turns::{Report, Tracker};
use crate::dogfood::catalog_config::{documents_in, CatalogEnvironment};
use grapt_api::DogfoodEventSubscription;
use grapt_stateengine_ipc::{Activity, AgentId, AgentSelector, SessionId, Sha256Hex};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn frame(kind: &str, data: serde_json::Value) -> Vec<u8> {
    let event = json!({"type": kind, "id": "evt_fixture", "created": 1500.0, "data": data});
    format!("id: evt_fixture\ndata: {event}\n\n").into_bytes()
}

fn status(session: &str, state: &str) -> Vec<u8> {
    frame(
        "session.status",
        json!({"sessionID": session, "status": {"type": state}}),
    )
}

fn text(session: &str, text: &str) -> Vec<u8> {
    frame(
        "session.text.ended",
        json!({"sessionID": session, "assistantMessageID": "msg_fixture", "text": text}),
    )
}

fn execution(kind: &str, session: &str, execution: &str) -> Vec<u8> {
    frame(
        &format!("session.execution.{kind}"),
        json!({"sessionID": session, "executionID": execution}),
    )
}

#[test]
fn only_the_users_own_documents_name_a_seat() -> TestResult {
    let root = tempfile::tempdir()?;
    let environment = CatalogEnvironment::isolated(root.path());
    let (service, config) = (root.path().join("service"), root.path().join("config"));
    std::fs::create_dir_all(&service)?;
    std::fs::create_dir_all(&config)?;
    let seat = || from_documents(&documents_in(&environment)?);
    // No declaration: a standalone service starts nothing.
    assert_eq!(seat()?, None);
    let state = root.path().join("state");
    let declared = |home: &str, agent: &str| {
        json!({"stateengine_seat": {"home": home, "binary": state.join("se"),
            "project": "prj_fixture", "agent": agent}})
        .to_string()
    };
    let home = state.to_string_lossy();
    std::fs::write(service.join("grapt.json"), declared(&home, "agent-fixture"))?;
    std::fs::write(config.join("grapt.json"), declared(&home, "ag_fixture"))?;
    let config_seat = seat()?.ok_or("the user's seat was not read")?;
    assert_eq!(
        config_seat.agent,
        AgentSelector::Id(AgentId("ag_fixture".into()))
    );
    // A relative home is refused.
    std::fs::write(
        config.join("grapt.json"),
        declared("state", "agent-fixture"),
    )?;
    assert!(seat().is_err());
    std::fs::remove_file(config.join("grapt.json"))?;
    assert!(seat()?.is_some());
    // A profile inside a repository holds the repository's authority.
    std::fs::create_dir(root.path().join(".git"))?;
    assert!(seat().is_err());
    Ok(())
}

#[test]
fn the_seat_is_busy_while_any_session_is_and_a_seen_execution_is_one_turn() {
    let mut tracker = Tracker::default();
    let reports: Vec<_> = [
        status("ses_a", "busy"),
        status("ses_b", "busy"),
        execution("started", "ses_a", "exe_a"),
        text("ses_a", "first"),
        text("ses_a", "second"),
        execution("succeeded", "ses_a", "exe_a"),
        status("ses_a", "idle"),
        execution("started", "ses_b", "exe_b"),
        text("ses_b", "abandoned"),
        execution("interrupted", "ses_b", "exe_b"),
        // After a gap: no start was seen, so this text is not the whole turn.
        text("ses_b", "suffix"),
        execution("succeeded", "ses_b", "exe_c"),
        // A start for another execution never completes this one.
        execution("started", "ses_b", "exe_d"),
        text("ses_b", "other"),
        execution("succeeded", "ses_b", "exe_e"),
        status("ses_b", "idle"),
        status("ses_c", "busy"),
    ]
    .iter()
    .filter_map(|frame| tracker.observe(frame))
    .collect();
    // A feed that ends while a session is busy says so once.
    assert_eq!(tracker.activity(), Activity::Busy);
    let body = "first\n\nsecond";
    let turn = SeatTurn {
        conversation: "ses_a".into(),
        vendor_turn: "exe_a".into(),
        occurred_at_us: 1_500_000,
        provider: String::new(),
        model: String::new(),
        body: Body::Whole(body.into()),
        digest: Sha256Hex(Sha256::digest(body).into()),
    };
    assert_eq!(
        reports,
        [
            Report::Activity(Activity::Busy),
            Report::Turn(turn),
            Report::Activity(Activity::Idle),
            Report::Activity(Activity::Busy),
        ]
    );
}

#[test]
fn an_elided_slice_beside_others_publishes_no_guessed_digest() {
    let mut tracker = Tracker::default();
    let elided = frame(
        "session.text.ended",
        json!({"sessionID": "ses_a", "text": "", "textBytes": 9, "textSha256": "ab".repeat(32)}),
    );
    let started = execution("started", "ses_a", "exe_a");
    let succeeded = execution("succeeded", "ses_a", "exe_a");
    let reports: Vec<_> = [started, text("ses_a", "kept"), elided, succeeded]
        .iter()
        .filter_map(|frame| tracker.observe(frame))
        .collect();
    assert!(reports.is_empty(), "{reports:?}");
}

/// Answers like a host with one active external seat, opening a new seat
/// session on each report as a reconnected host would, or refuses like one
/// that is down.
#[derive(Default)]
struct Host {
    refused_reports: usize,
    refused_turns: usize,
    reports: Vec<Activity>,
    published: Vec<(SessionId, String)>,
}

impl SeatHost for Host {
    async fn activity(&mut self, state: Activity) -> Result<SeatSession, String> {
        refuse(&mut self.refused_reports)?;
        self.reports.push(state);
        Ok(SeatSession {
            agent: AgentId("ag_fixture".into()),
            session: SessionId(format!("ses_seat_{}", self.reports.len())),
        })
    }

    async fn turn(&mut self, seat: &SeatSession, turn: &SeatTurn) -> Result<(), String> {
        refuse(&mut self.refused_turns)?;
        self.published
            .push((seat.session.clone(), turn.vendor_turn.clone()));
        Ok(())
    }
}

fn refuse(remaining: &mut usize) -> Result<(), String> {
    if *remaining == 0 {
        return Ok(());
    }
    *remaining -= 1;
    Err("the StateEngine connection failed".into())
}

fn turn(vendor: &str) -> Command {
    Command::Turn(SeatTurn {
        conversation: "ses_a".into(),
        vendor_turn: vendor.into(),
        occurred_at_us: 1,
        provider: "provider-fixture".into(),
        model: "model-fixture".into(),
        body: Body::Omitted(4),
        digest: Sha256Hex([0; 32]),
    })
}

#[tokio::test(start_paused = true)]
async fn a_refused_report_is_retried_until_the_host_holds_the_latest_state() {
    let desired = Arc::new(Desired::default());
    let mut publisher = Publisher::new(Host::default(), Arc::clone(&desired));
    // No state is known yet, so a turn has no seat session to go to.
    publisher.apply(turn("exe_lost")).await;
    desired.set(Activity::Busy);
    publisher.apply(Command::Reconcile).await;
    assert_eq!(publisher.retry_in(), None);
    publisher.apply(turn("exe_a")).await;
    // A dropped edge: idle was never queued, and the host refuses once.
    desired.set(Activity::Idle);
    publisher.host_mut().refused_reports = 1;
    publisher.apply(Command::Reconcile).await;
    assert_eq!(publisher.retry_in(), Some(Duration::from_secs(1)));
    tokio::time::advance(Duration::from_secs(1)).await;
    publisher.apply(Command::Reconcile).await;
    assert_eq!(publisher.retry_in(), None);
    let host = publisher.host();
    assert_eq!(host.reports, [Activity::Busy, Activity::Idle]);
    assert_eq!(
        host.published,
        [(SessionId("ses_seat_1".into()), "exe_a".into())]
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_turn_is_published_on_a_freshly_reported_session() {
    let desired = Arc::new(Desired::default());
    let host = Host {
        refused_turns: 1,
        ..Host::default()
    };
    let mut publisher = Publisher::new(host, Arc::clone(&desired));
    desired.set(Activity::Busy);
    publisher.apply(turn("exe_a")).await;
    let host = publisher.host();
    assert_eq!(host.reports, [Activity::Busy, Activity::Busy]);
    assert_eq!(
        host.published,
        [(SessionId("ses_seat_2".into()), "exe_a".into())]
    );
}

/// A hub with one subscriber slot that restates `ses_hydrated` as busy.
fn busy_hub() -> Result<Arc<grapt_api::DogfoodEventHub>, Box<dyn std::error::Error>> {
    use grapt_api::u3a::{
        DogfoodEvent, FiniteF64, LiveEvent, ServerConnectedEvent, SessionStatusData,
        SessionStatusInfo, SseLimitValues, SseLimits, WireId,
    };
    let limits = SseLimits::try_new(SseLimitValues {
        subscribers: 1,
        pending_events: 16,
        event_bytes: 65_536,
        frame_bytes: 65_536,
    })?;
    let connected = ServerConnectedEvent::new(WireId::parse("evt_connected".into()).ok_or("id")?);
    let hub = grapt_api::DogfoodEventHub::try_new(limits, connected)?;
    let busy = DogfoodEvent::SessionStatus(LiveEvent::new(
        WireId::parse("evt_hydrated".into()).ok_or("id")?,
        FiniteF64::try_new(1.0)?,
        SessionStatusData::new(
            WireId::parse("ses_hydrated".into()).ok_or("id")?,
            SessionStatusInfo::busy(),
        ),
    ));
    hub.install_status_hydrator(Box::new(move || vec![busy.clone()]));
    Ok(Arc::new(hub))
}

#[test]
fn the_observer_restates_before_deciding_waits_for_room_and_stops_idle() -> TestResult {
    let hub = busy_hub()?;
    // The only slot is taken, so the observer waits for room.
    let held = hub.subscribe_host()?;
    let stop = Arc::new(super::Stop::default());
    let desired = Arc::new(Desired::default());
    let (commands, received) = std::sync::mpsc::sync_channel(8);
    let observer = {
        let (hub, stop, desired) = (Arc::clone(&hub), Arc::clone(&stop), Arc::clone(&desired));
        std::thread::spawn(move || super::observe(&hub, &stop, &desired, &commands))
    };
    drop(held);
    let first = subscribed(&stop, None)?;
    hub.reconcile_because(
        grapt_api::u3a::ReconcileReason::UnpublishedAdvisoryNotice,
        "fixture",
    );
    // A reconciliation ends the stream; the next subscription restates the
    // busy session before the seat's state is decided, so idle is never sent.
    subscribed(&stop, Some(&first))?;
    let wait = Duration::from_secs(10);
    for _ in 0..2 {
        assert_eq!(received.recv_timeout(wait)?, Command::Reconcile);
        assert_eq!(desired.get(), Some(Activity::Busy));
    }
    stop.stop();
    observer.join().map_err(|_| "the observer panicked")?;
    assert!(received.try_recv().is_err());
    assert_eq!(desired.get(), Some(Activity::Idle));
    Ok(())
}

#[test]
fn every_busy_session_is_restated_so_the_seat_stays_busy_past_the_idle_cap() -> TestResult {
    use crate::dogfood::session_status::SessionStatusEmitter;
    let hub = crate::dogfood::event_hub()?;
    let ids = Arc::new(grapt_store::OsRandomIds::seed()?);
    let emitter = Arc::new(SessionStatusEmitter::new(
        ids,
        Arc::new(grapt_store::SystemClock),
        Arc::clone(&hub),
    ));
    let sessions = (0..40)
        .map(|index| format!("ses_busy_{index:02}").parse())
        .collect::<Result<Vec<grapt_types::SessionId>, _>>()?;
    let (hydrating, listed) = (Arc::clone(&emitter), sessions.clone());
    hub.install_status_hydrator(Box::new(move || hydrating.hydrate(&listed)));
    let subscription = hub.subscribe_host()?;
    assert_eq!(subscription.hydration_frames(), 40);
    let mut tracker = Tracker::default();
    let read = |tracker: &mut Tracker, count: usize| -> Result<(), Box<dyn std::error::Error>> {
        for _ in 0..count {
            let frame = subscription.next_frame()?.ok_or("the feed ended")?;
            tracker.observe(&frame);
        }
        Ok(())
    };
    read(&mut tracker, 41)?;
    // The first 32 finish while eight are still running.
    for session in &sessions[..32] {
        emitter.publish_idle(session);
    }
    read(&mut tracker, 32)?;
    assert_eq!(tracker.activity(), Activity::Busy);
    for session in &sessions[32..] {
        emitter.publish_idle(session);
    }
    read(&mut tracker, 8)?;
    assert_eq!(tracker.activity(), Activity::Idle);
    Ok(())
}

#[test]
fn a_seat_whose_thread_will_not_end_stops_within_its_bound() -> TestResult {
    let (ending, ended) = std::sync::mpsc::channel::<()>();
    let (release, stuck) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let _ending = ending;
        let _ = stuck.recv();
    });
    let seat = super::Seat {
        stop: Arc::new(super::Stop::default()),
        threads: vec![thread],
        ended,
    };
    // Returns after the bound although the thread is still running.
    drop(seat);
    release.send(())?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_directory_linked_out_of_a_repository_still_belongs_to_it() -> TestResult {
    let root = tempfile::tempdir()?;
    let mut environment = CatalogEnvironment::isolated(root.path());
    let (repo, outside) = (root.path().join("repo"), root.path().join("outside"));
    std::fs::create_dir_all(repo.join(".git"))?;
    std::fs::create_dir_all(&outside)?;
    std::fs::create_dir_all(root.path().join("service"))?;
    let declared = json!({"stateengine_seat": {"home": outside, "binary": outside.join("se"),
        "project": "prj_fixture", "agent": "agent-fixture"}});
    std::fs::write(outside.join("grapt.json"), declared.to_string())?;
    std::os::unix::fs::symlink(&outside, repo.join("link"))?;
    // Resolved, the file lies outside the checkout; as written, inside it.
    environment.config = repo.join("link");
    assert!(from_documents(&documents_in(&environment)?).is_err());
    environment.config = outside;
    assert!(from_documents(&documents_in(&environment)?)?.is_some());
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_linked_user_document_is_refused() -> TestResult {
    let root = tempfile::tempdir()?;
    let environment = CatalogEnvironment::isolated(root.path());
    std::fs::create_dir_all(root.path().join("service"))?;
    std::fs::create_dir_all(root.path().join("elsewhere"))?;
    let target = root.path().join("elsewhere/grapt.json");
    std::fs::write(&target, r#"{"stateengine_seat": {}}"#)?;
    std::os::unix::fs::symlink(&target, root.path().join("service/grapt.json"))?;
    assert!(documents_in(&environment).is_err());
    Ok(())
}

/// The observer's current subscription once it differs from `after`.
fn subscribed(
    stop: &super::Stop,
    after: Option<&Arc<DogfoodEventSubscription>>,
) -> Result<Arc<DogfoodEventSubscription>, String> {
    for _ in 0..10_000_000 {
        let state = stop.current();
        if let Some(current) = &*state {
            if after.is_none_or(|old| !Arc::ptr_eq(old, current)) {
                return Ok(Arc::clone(current));
            }
        }
        drop(state);
        std::thread::yield_now();
    }
    Err("the observer never subscribed".into())
}
