use super::*;
use grapt_client::client::SessionActivity;
use grapt_client::{projection::FactStamp, wire::SessionWireId};

#[derive(Debug)]
struct ActivityPeer {
    inner: ContractDouble,
    frame: Option<FramePoll>,
    session: SessionWireId,
    failed: bool,
}

/// The failure row a failed execution leaves in history.
const FAILURE_PAGE: &[u8] = br#"{"data":[{"type":"system","id":"msg_activity_failure","metadata":{"executionFailure":true},"time":{"created":101},"text":"Execution failed: provider refused"}],"cursor":{}}"#;

impl Transport for ActivityPeer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if self.failed && request.operation() == Operation::SessionSnapshot {
            // The snapshot at the failure's committed position.
            let body = grapt_client::double::page_snapshot(request, FAILURE_PAGE, 12).map_err(
                |error| TransportError::Unavailable {
                    reason: error.to_string(),
                },
            )?;
            return Ok(WireResponse { status: 200, body });
        }
        self.inner.call(request)
    }
}

impl Connection for ActivityPeer {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        if let Some(connect) = self.inner.connect() {
            return Ok(connect);
        }
        Ok(self
            .frame
            .take()
            .map_or(FramePoll::Idle, |frame| self.inner.serve(frame)))
    }
    fn subscribed(&self) -> bool {
        true
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
}

fn client() -> Result<Client<ActivityPeer>, Box<dyn std::error::Error>> {
    let mut inner = ContractDouble::new();
    let session = inner.create_session("activity ordering").ok_or("session")?;
    let mut client = Client::new(
        ActivityPeer {
            inner,
            frame: None,
            session: session.clone(),
            failed: false,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_sessions()?;
    client.select(&session)?;
    // The stream's positioned connect, then the snapshot it permits.
    for _ in 0..2 {
        client.pump_at(Duration::ZERO)?;
    }
    Ok(client)
}

fn frame(
    client: &mut Client<ActivityPeer>,
    kind: &str,
    seq: Option<u64>,
    created: u64,
) -> TestResult {
    if kind == "failed" {
        client.peer_mut().failed = true;
    }
    let session = client.peer_mut().session.clone();
    let (member, data) = match kind {
        // A status edge carries its typed state at committed position `seq`.
        "busy" | "idle" => (
            "session.status".to_owned(),
            serde_json::json!({"sessionID":session.spelled(),"status":{"type":kind},
            "state": if kind == "busy" {
                serde_json::json!({"type":"running","revision":seq,"executionID":"exe_activity"})
            } else {
                serde_json::json!({"type":"idle","revision":seq})
            }}),
        ),
        _ => (
            format!(
                "session.execution.{}",
                if kind == "restart" { "started" } else { kind }
            ),
            serde_json::json!({"sessionID":session.spelled(),
                "executionID":if kind == "restart" { "exe_next" } else { "exe_activity" }}),
        ),
    };
    let mut value = serde_json::json!({"id":format!("evt_activity_{kind}_{created}"),
        "type":member,"created":created,"data":data});
    if let Some(seq) = seq.filter(|_| !matches!(kind, "busy" | "idle")) {
        value["durable"] =
            serde_json::json!({"aggregateID":session.spelled(),"seq":seq,"version":1});
    }
    client.peer_mut().frame = Some(FramePoll::Frame(value.to_string()));
    client.pump_at(Duration::ZERO)?;
    Ok(())
}

#[test]
fn review_ordered_activity_handles_both_directions_and_settled_ties() -> TestResult {
    let scripts = [
        (
            vec![("started", Some(10), 100), ("failed", Some(12), 101)],
            false,
        ),
        (
            vec![
                ("started", Some(10), 100),
                ("failed", Some(12), 101),
                ("busy", Some(13), 105),
            ],
            true,
        ),
        (
            vec![
                ("idle", Some(9), 90),
                ("started", Some(10), 100),
                ("failed", Some(12), 101),
            ],
            false,
        ),
        (
            vec![
                ("started", Some(10), 90),
                ("failed", Some(12), 200),
                ("restart", Some(20), 100),
            ],
            true,
        ),
        (
            vec![
                ("started", Some(10), 100),
                ("busy", Some(11), 101),
                ("failed", Some(12), 101),
            ],
            false,
        ),
    ];
    for (script, running) in scripts {
        let mut client = client()?;
        for (kind, seq, created) in script.iter().copied() {
            frame(&mut client, kind, seq, created)?;
        }
        assert_eq!(selected_activity(&client), Some(running), "{script:?}");
        assert_eq!(selected_running(&client), running);
    }
    Ok(())
}

#[test]
fn review_a_live_failure_clears_the_thinking_label_without_an_idle_frame() -> TestResult {
    let mut client = client()?;
    frame(&mut client, "started", Some(10), 100)?;
    let mut ui = crate::ui::Ui::new(false);
    ui.session_update(client.selected().cloned(), selected_running(&client));
    let now = tokio::time::Instant::now();
    ui.activity
        .observe(ui.running(), None, ui.activity_phase(), now);
    assert!(ui.activity_label().is_some());
    frame(&mut client, "failed", Some(12), 101)?;
    assert!(client
        .view()
        .ok_or("view")?
        .slices
        .iter()
        .any(|slice| slice.text.contains("Execution failed")));
    ui.session_update(client.selected().cloned(), selected_running(&client));
    ui.activity
        .observe(ui.running(), None, ui.activity_phase(), now);
    assert!(!ui.running());
    assert!(ui.activity_label().is_none());
    Ok(())
}

fn picker_last_fact(
    kind: &str,
    seq: Option<u64>,
    created: u64,
    expected: SessionActivity,
) -> TestResult {
    let mut client = client()?;
    let parent = client.selected().ok_or("parent")?.clone();
    let child = client
        .peer_mut()
        .inner
        .create_session("background activity")
        .ok_or("child")?;
    client.refresh_sessions()?;
    for (kind, seq, created) in [
        ("started", Some(10), 90),
        ("failed", Some(12), 100),
        (kind, seq, created),
    ] {
        let (member, data) = if kind == "busy" {
            (
                "session.status".to_owned(),
                serde_json::json!({"sessionID":child.spelled(),"status":{"type":"busy"}}),
            )
        } else {
            (
                format!("session.execution.{kind}"),
                serde_json::json!({"sessionID":child.spelled(),
                "executionID":if seq == Some(20) { "exe_next" } else { "exe_activity" }}),
            )
        };
        let mut value = serde_json::json!({"id":format!("evt_picker_{kind}_{created}"),
            "type":member,"created":created,"data":data});
        if let Some(seq) = seq {
            value["durable"] =
                serde_json::json!({"aggregateID":child.spelled(),"seq":seq,"version":1});
        }
        client.peer_mut().frame = Some(FramePoll::Frame(value.to_string()));
        client.pump_at(Duration::ZERO)?;
    }
    assert_eq!(client.selected(), Some(&parent));
    assert!(
        client.view_of(&child).is_none(),
        "picker facts need no history install"
    );
    assert_eq!(client.observed_activity(&child), Some(expected));
    Ok(())
}

#[test]
fn review_picker_sequence_wins_over_created() -> TestResult {
    picker_last_fact("started", Some(20), 99, SessionActivity::Running)
}

#[test]
fn review_picker_settled_activity_wins_timestamp_tie() -> TestResult {
    picker_last_fact("busy", None, 100, SessionActivity::Failed)
}

#[test]
fn review_presented_pairs_order_running_and_failed_against_live_facts() -> TestResult {
    let mut client = client()?;
    for (revision, kind, running) in [(20, "running", true), (30, "failed", false)] {
        let session = client.peer_mut().session.clone();
        let member = if running { "failed" } else { "started" };
        let execution = if running { "exe_activity" } else { "exe_next" };
        client.peer_mut().frame = Some(FramePoll::Positioned {
            id: format!("activity-live-{revision}"),
            data: serde_json::json!({"id":format!("evt_activity_live_{revision}"),
                "type":format!("session.execution.{member}"),"created":revision,
                "durable":{"aggregateID":session.spelled(),"seq":revision-1,"version":1},
                "data":{"sessionID":session.spelled(),"executionID":execution}})
            .to_string(),
        });
        client.pump_at(Duration::ZERO)?;
        assert_eq!(selected_activity(&client), Some(!running));
        client.peer_mut().frame = Some(FramePoll::Positioned {
            id: format!("activity-{revision}"),
            data: serde_json::json!({"id":format!("evt_activity_pair_{revision}"),
                "type":"session.status","created":revision,"data":{"sessionID":session.spelled(),
                    "status":{"type":"busy"},"state":{"type":kind,"revision":revision,
                    "executionID":execution}}})
            .to_string(),
        });
        client.pump_at(Duration::ZERO)?;
        assert_eq!(selected_activity(&client), Some(running));
        assert_eq!(
            client.view().ok_or("view")?.idle_stamp().seq,
            Some(revision)
        );
    }
    Ok(())
}

#[test]
fn review_fact_stamps_use_only_the_common_server_order() {
    use std::cmp::Ordering;
    let stamp = |seq, created| FactStamp { seq, created };
    assert_eq!(
        stamp(Some(20), Some(100)).order(&stamp(Some(12), Some(200))),
        Some(Ordering::Greater)
    );
    assert_eq!(
        stamp(Some(20), Some(100)).order(&stamp(None, Some(200))),
        Some(Ordering::Less)
    );
    assert_eq!(stamp(Some(20), None).order(&stamp(None, Some(200))), None);
    assert_eq!(
        stamp(None, Some(200)).order(&stamp(None, Some(200))),
        Some(Ordering::Equal)
    );
}
