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

impl Transport for ActivityPeer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if self.failed && request.operation() == Operation::MessageList {
            return Ok(WireResponse {
                status: 200,
                body: br#"{"data":[{"type":"system","id":"msg_activity_failure","metadata":{"executionFailure":true},"time":{"created":101},"text":"Execution failed: provider refused"}],"cursor":{}}"#.to_vec(),
            });
        }
        if request.operation() == Operation::SessionSnapshot {
            return Ok(WireResponse {
                status: 200,
                body: serde_json::json!({
                    "revision":{"aggregate":self.session.spelled(),"seq":1},
                    "rows":[],"cursor":{},"permissions":[],"forms":[],
                    "execution":{},"provisional":[]
                })
                .to_string()
                .into_bytes(),
            });
        }
        self.inner.call(request)
    }
}

impl Connection for ActivityPeer {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        Ok(self.frame.take().unwrap_or(FramePoll::Idle))
    }
    fn subscribed(&self) -> bool {
        true
    }
    fn resubscribe(&mut self) {}
}

fn client(history: bool) -> Result<Client<ActivityPeer>, Box<dyn std::error::Error>> {
    let mut inner = ContractDouble::new();
    let session = inner.create_session("activity ordering").ok_or("session")?;
    if history {
        inner.run_turn(&session, "hello", "done");
    }
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
        "busy" | "idle" => (
            "session.status".to_owned(),
            serde_json::json!({"sessionID":session.spelled(),"status":{"type":kind}}),
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
    if let Some(seq) = seq {
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
                ("busy", None, 105),
            ],
            true,
        ),
        (
            vec![
                ("idle", None, 90),
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
                ("busy", None, 101),
                ("failed", Some(12), 101),
            ],
            false,
        ),
    ];
    for (script, running) in scripts {
        let mut client = client(false)?;
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
    let mut client = client(false)?;
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
    let mut client = client(false)?;
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
fn review_an_unstamped_history_terminal_keeps_legacy_busy_precedence() -> TestResult {
    let mut client = client(true)?;
    assert!(matches!(
        client.view().ok_or("view")?.execution,
        grapt_client::ExecutionState::Settled { .. }
    ));
    frame(&mut client, "busy", None, 105)?;
    let session = client.selected().ok_or("session")?.clone();
    client.reconcile(&session)?;
    assert_eq!(
        client.view().ok_or("view")?.terminal_stamp(),
        FactStamp::default()
    );
    assert_eq!(selected_activity(&client), Some(true));
    Ok(())
}

#[test]
fn review_presented_pairs_order_running_and_failed_against_live_facts() -> TestResult {
    let mut client = client(false)?;
    client.peer_mut().frame = Some(FramePoll::Positioned {
        id: "activity-epoch".into(),
        data:
            serde_json::json!({"id":"evt_activity_connected","type":"server.connected","data":{}})
                .to_string(),
    });
    client.pump_at(Duration::ZERO)?;
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
