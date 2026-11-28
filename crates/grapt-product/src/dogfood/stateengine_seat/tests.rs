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
    assert!(tracker.release());
    assert!(!tracker.release());
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

#[test]
fn the_observer_outlives_a_reconciliation_and_stops_idle() -> TestResult {
    let hub = crate::dogfood::event_hub()?;
    let stop = Arc::new(super::Stop::default());
    let desired = Arc::new(Desired::default());
    desired.set(Activity::Busy);
    let (commands, _received) = std::sync::mpsc::sync_channel(1);
    let observer = {
        let (hub, stop, desired) = (Arc::clone(&hub), Arc::clone(&stop), Arc::clone(&desired));
        std::thread::spawn(move || super::observe(&hub, &stop, &desired, &commands))
    };
    let first = subscribed(&stop, None)?;
    hub.reconcile();
    // A reconciliation ends the stream; the observer subscribes again.
    subscribed(&stop, Some(&first))?;
    assert!(!observer.is_finished());
    stop.stop();
    observer.join().map_err(|_| "the observer panicked")?;
    assert_eq!(desired.get(), Some(Activity::Idle));
    Ok(())
}

/// The observer's current subscription once it differs from `after`.
fn subscribed(
    stop: &super::Stop,
    after: Option<&Arc<DogfoodEventSubscription>>,
) -> Result<Arc<DogfoodEventSubscription>, String> {
    for _ in 0..10_000_000 {
        let state = stop.0.lock().map_err(|_| "stop state poisoned")?;
        if let Some(current) = &state.current {
            if after.is_none_or(|old| !Arc::ptr_eq(old, current)) {
                return Ok(Arc::clone(current));
            }
        }
        drop(state);
        std::thread::yield_now();
    }
    Err("the observer never subscribed".into())
}
