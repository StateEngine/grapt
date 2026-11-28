use super::config::from_documents;
use super::publisher::{Body, Command, Publisher, SeatHost, SeatSession, SeatTurn};
use super::turns::Tracker;
use crate::dogfood::catalog_config::{CatalogEnvironment, CatalogSources};
use grapt_stateengine_ipc::{Activity, AgentId, AgentSelector, SessionId, Sha256Hex};
use serde_json::json;
use sha2::{Digest, Sha256};

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

fn text(session: &str, ordinal: u64, text: &str) -> Vec<u8> {
    frame(
        "session.text.ended",
        json!({"sessionID": session, "assistantMessageID": "msg_fixture", "ordinal": ordinal, "text": text}),
    )
}

fn succeeded(session: &str, execution: &str) -> Vec<u8> {
    frame(
        "session.execution.succeeded",
        json!({"sessionID": session, "executionID": execution}),
    )
}

#[test]
fn only_a_user_owned_declaration_names_a_seat() -> TestResult {
    let root = tempfile::tempdir()?;
    let (service, repo) = (root.path().join("service"), root.path().join("repo"));
    std::fs::create_dir_all(&service)?;
    std::fs::create_dir_all(&repo)?;
    let seat = || -> Result<_, String> {
        from_documents(
            &CatalogSources::read(&repo, CatalogEnvironment::isolated(root.path()))?.documents,
        )
    };
    // No declaration: a standalone service starts nothing.
    assert_eq!(seat()?, None);
    let declared = json!({"stateengine_seat": {"home": "/state/home", "binary": "/state/se",
        "project": "prj_fixture", "agent": "agent-fixture"}});
    std::fs::write(service.join("grapt.json"), declared.to_string())?;
    let config = seat()?.ok_or("the user's seat was not read")?;
    assert_eq!(config.agent, AgentSelector::Name("agent-fixture".into()));
    // A repository can never start one, and a relative home is refused.
    std::fs::write(repo.join("grapt.json"), declared.to_string())?;
    assert!(seat().is_err());
    std::fs::remove_file(repo.join("grapt.json"))?;
    let relative = json!({"stateengine_seat": {"home": "state", "binary": "/state/se",
        "project": "prj_fixture", "agent": "agent-fixture"}});
    std::fs::write(service.join("grapt.json"), relative.to_string())?;
    assert!(seat().is_err());
    Ok(())
}

#[test]
fn the_seat_is_busy_while_any_session_is_and_a_success_is_one_turn() {
    let mut tracker = Tracker::default();
    let mut commands = Vec::new();
    for frame in [
        status("ses_a", "busy"),
        status("ses_b", "busy"),
        text("ses_a", 0, "first"),
        text("ses_a", 1, "second"),
        succeeded("ses_a", "exe_a"),
        status("ses_a", "idle"),
        text("ses_b", 0, "abandoned"),
        frame(
            "session.execution.interrupted",
            json!({"sessionID": "ses_b"}),
        ),
        status("ses_b", "idle"),
    ] {
        commands.extend(tracker.observe(&frame));
    }
    // A feed that ends while a session is busy owes the host an idle report.
    commands.extend(tracker.observe(&status("ses_c", "busy")));
    assert_eq!(tracker.release(), Some(Command::Activity(Activity::Idle)));
    assert_eq!(tracker.release(), None);
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
        commands,
        [
            Command::Activity(Activity::Busy),
            Command::Turn(turn),
            Command::Activity(Activity::Idle),
            Command::Activity(Activity::Busy),
        ]
    );
}

#[test]
fn an_elided_slice_beside_others_publishes_no_guessed_digest() {
    let mut tracker = Tracker::default();
    let elided = frame(
        "session.text.ended",
        json!({"sessionID": "ses_a", "assistantMessageID": "msg_fixture", "ordinal": 1,
            "text": "", "textBytes": 9, "textSha256": "ab".repeat(32)}),
    );
    let mut commands = Vec::new();
    for frame in [
        text("ses_a", 0, "kept"),
        elided,
        succeeded("ses_a", "exe_a"),
    ] {
        commands.extend(tracker.observe(&frame));
    }
    assert!(commands.is_empty(), "{commands:?}");
}

/// Answers like a host with one active external seat, or refuses like one
/// that is down.
#[derive(Default)]
struct Host {
    /// Calls refused before the host comes back.
    failures: usize,
    published: Vec<(SeatSession, String)>,
    reports: Vec<Activity>,
}

impl SeatHost for Host {
    async fn activity(&mut self, state: Activity) -> Result<SeatSession, String> {
        self.refuse()?;
        self.reports.push(state);
        Ok(SeatSession {
            agent: AgentId("ag_fixture".into()),
            session: SessionId("ses_seat_fixture".into()),
        })
    }

    async fn turn(&mut self, seat: &SeatSession, turn: &SeatTurn) -> Result<(), String> {
        self.refuse()?;
        self.published
            .push((seat.clone(), turn.vendor_turn.clone()));
        Ok(())
    }
}

impl Host {
    fn refuse(&mut self) -> Result<(), String> {
        if self.failures == 0 {
            return Ok(());
        }
        self.failures -= 1;
        Err("the StateEngine connection failed".into())
    }
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

#[tokio::test]
async fn a_down_host_costs_reports_but_never_blocks_and_recovery_resumes() {
    let mut publisher = Publisher::new(Host {
        failures: 2,
        ..Host::default()
    });
    publisher.apply(Command::Activity(Activity::Busy)).await;
    // No seat session is known, so the turn is skipped, never queued.
    publisher.apply(turn("exe_lost")).await;
    publisher.apply(Command::Activity(Activity::Idle)).await;
    // The host is back: the next turn's reports and publication go through.
    publisher.apply(Command::Activity(Activity::Busy)).await;
    publisher.apply(turn("exe_kept")).await;
    let host = publisher.host();
    assert_eq!(host.reports, [Activity::Busy]);
    assert_eq!(host.published.len(), 1);
    assert_eq!(
        host.published[0].0.session,
        SessionId("ses_seat_fixture".into())
    );
}
