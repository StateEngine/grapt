//! A live block's snapshot and retained stream form one text, never a prefix.
use grapt_client::{
    bounds::Bounds,
    client::Connection,
    contract::Operation,
    double::ContractDouble,
    transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse},
    wire::{SessionWireId, Spelled},
    Client,
};
use serde_json::json;
use std::collections::VecDeque;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const MESSAGE: &str = "msg_snapshot_join";

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    frames: VecDeque<FramePoll>,
    snapshot: WireResponse,
    snapshots: usize,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if request.operation() == Operation::SessionSnapshot {
            self.snapshots += 1;
            return Ok(self.snapshot.clone());
        }
        self.inner.call(request)
    }
}
impl Connection for Peer {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        Ok(self.frames.pop_front().unwrap_or(FramePoll::Idle))
    }
    fn resubscribe(&mut self) {}
    fn subscribed(&self) -> bool {
        true
    }
}
fn connected() -> FramePoll {
    FramePoll::Positioned {
        id: "epoch-1".into(),
        data: json!({"id":"evt_join_connected","type":"server.connected","data":{}}).to_string(),
    }
}
fn client(prefix: Option<&str>) -> TestResult<(Client<Peer>, SessionWireId)> {
    client_bounded(prefix, Bounds::PRODUCTION)
}
fn client_bounded(
    prefix: Option<&str>,
    bounds: Bounds,
) -> TestResult<(Client<Peer>, SessionWireId)> {
    client_at(prefix, bounds, 100)
}
fn client_at(
    prefix: Option<&str>,
    bounds: Bounds,
    position: u64,
) -> TestResult<(Client<Peer>, SessionWireId)> {
    let mut inner = ContractDouble::new();
    let session = inner
        .create_session("snapshot join")
        .ok_or("session bound")?;
    let rows = inner.call(&WireRequest::new(
        Operation::MessageList,
        Some(&session),
        None,
    )?)?;
    let page: serde_json::Value = serde_json::from_slice(&rows.body)?;
    let provisional: Vec<_> = prefix.into_iter().map(|text| json!({
        "assistantMessageID":MESSAGE,"kind":"text","ordinal":0,"text":text,"bytes":text.len()
    })).collect();
    let snapshot = WireResponse {
        status: 200,
        body: serde_json::to_vec(&json!({
            "revision":{"aggregate":session.spelled(),"seq":position},"rows":page["data"],
            "cursor":page["cursor"],"permissions":[],"forms":[],"execution":{},"provisional":provisional
        }))?,
    };
    let mut client = Client::new(
        Peer {
            inner,
            frames: VecDeque::from([connected()]),
            snapshot,
            snapshots: 0,
        },
        bounds,
    );
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project offer")?;
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent offer")?.id.clone();
    client.select_agent(&agent)?;
    let model = client
        .models()
        .iter()
        .find(|row| row.enabled)
        .cloned()
        .ok_or("model offer")?;
    client.select_model(&model)?;
    client.pump()?;
    client.select(&session)?;
    assert_eq!(
        client.peer_mut().snapshots,
        1,
        "fixture did not read its snapshot"
    );
    Ok((client, session))
}
fn block_frame(session: &SessionWireId, phase: &str, text: &str, from: Option<u64>) -> FramePoll {
    let mut value = json!({"id":format!("evt_join_{phase}"),"type":format!("session.text.{phase}"),"created":1,
        "data":{"sessionID":session.spelled(),"assistantMessageID":MESSAGE,"ordinal":0}});
    if phase == "delta" {
        value["data"]["delta"] = json!(text);
        if let Some(from) = from {
            value["data"]["from"] = json!(from);
        }
    } else {
        value["durable"] = json!({"aggregateID":session.spelled(),"seq":101,"version":1});
        value["data"]["text"] = json!(text);
    }
    FramePoll::Positioned {
        id: "epoch-2".into(),
        data: value.to_string(),
    }
}
fn rendered(client: &Client<Peer>) -> String {
    client
        .view()
        .map(|view| {
            view.slices
                .iter()
                .map(|slice| slice.text.as_str())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn retained_end_materializes_a_block_closed_between_p_and_capture() -> TestResult {
    let (mut client, session) = client(None)?;
    client
        .peer_mut()
        .frames
        .push_back(block_frame(&session, "ended", "é🙂x", None));
    client.pump()?;
    assert_eq!(
        rendered(&client),
        "é🙂x",
        "full retained end was lost after snapshot"
    );
    assert_eq!(
        client.peer_mut().snapshots,
        1,
        "full end should not require another snapshot"
    );
    Ok(())
}

#[test]
fn retained_end_completes_the_provisional_prefix() -> TestResult {
    let (mut client, session) = client(Some("é"))?;
    client
        .peer_mut()
        .frames
        .push_back(block_frame(&session, "ended", "é🙂x", None));
    client.pump()?;
    assert_eq!(
        rendered(&client),
        "é🙂x",
        "end silently sealed only the captured prefix"
    );
    assert!(client
        .view()
        .ok_or("view")?
        .slices
        .iter()
        .all(|slice| slice.closed));
    Ok(())
}

#[test]
fn positioned_delta_without_offset_recovers_instead_of_appending() -> TestResult {
    let (mut client, session) = client(Some("é"))?;
    client
        .peer_mut()
        .frames
        .push_back(block_frame(&session, "delta", "unpositioned", None));
    client.pump()?;
    assert!(
        !rendered(&client).contains("unpositioned"),
        "missing offset appended unplaced"
    );
    assert!(
        client.diagnostics().gaps_observed > 0,
        "missing offset did not request recovery"
    );
    Ok(())
}

#[test]
fn positioned_multibyte_overlap_keeps_only_the_unseen_tail() -> TestResult {
    let (mut client, session) = client(Some("é"))?;
    client
        .peer_mut()
        .frames
        .push_back(block_frame(&session, "delta", "é🙂x", Some(0)));
    client.pump()?;
    assert_eq!(rendered(&client), "é🙂x");
    assert_eq!(client.diagnostics().gaps_observed, 0);
    Ok(())
}

#[test]
fn coherent_snapshot_with_empty_authority_lists_installs() -> TestResult {
    let (client, _) = client(Some("é"))?;
    assert_eq!(rendered(&client), "é");
    assert!(client.stream_state().is_live());
    Ok(())
}

fn elided_end(session: &SessionWireId, text: &str) -> TestResult<FramePoll> {
    use sha2::Digest as _;
    let FramePoll::Positioned { id, data } = block_frame(session, "ended", "", None) else {
        return Err("positioned fixture end".into());
    };
    let mut value: serde_json::Value = serde_json::from_str(&data)?;
    value["data"]["textBytes"] = json!(text.len());
    value["data"]["textSha256"] = json!(format!("{:x}", sha2::Sha256::digest(text.as_bytes())));
    Ok(FramePoll::Positioned {
        id,
        data: value.to_string(),
    })
}
fn change_snapshot(
    client: &mut Client<Peer>,
    change: impl FnOnce(&mut serde_json::Value),
) -> TestResult {
    let mut value: serde_json::Value = serde_json::from_slice(&client.peer_mut().snapshot.body)?;
    change(&mut value);
    client.peer_mut().snapshot.body = serde_json::to_vec(&value)?;
    Ok(())
}
fn assert_closed(client: &Client<Peer>) -> TestResult {
    assert!(client
        .view()
        .ok_or("view")?
        .slices
        .iter()
        .all(|slice| slice.closed));
    Ok(())
}

#[test]
fn elided_end_proves_bytes_discarded_by_the_display_bound() -> TestResult {
    let bounds = Bounds {
        max_slice_bytes: 3,
        ..Bounds::PRODUCTION
    };
    for seeded in [false, true] {
        let (mut client, session) =
            client_bounded(Some(if seeded { "é🙂x" } else { "é" }), bounds)?;
        if !seeded {
            client
                .peer_mut()
                .frames
                .push_back(block_frame(&session, "delta", "🙂x", Some(2)));
            client.pump()?;
        }
        assert_eq!(rendered(&client), "é");
        assert_eq!(
            client
                .view()
                .ok_or("view")?
                .slices
                .last()
                .ok_or("slice")?
                .truncated_bytes,
            5
        );
        client
            .peer_mut()
            .frames
            .push_back(elided_end(&session, "é🙂x")?);
        client.pump()?;
        assert_closed(&client)?;
        assert_eq!(client.diagnostics().gaps_observed, 0);
        assert_eq!(client.peer_mut().snapshots, 1);
    }
    Ok(())
}

#[test]
fn equal_length_end_with_different_discarded_bytes_requires_recovery() -> TestResult {
    let bounds = Bounds {
        max_slice_bytes: 3,
        ..Bounds::PRODUCTION
    };
    for elided in [false, true] {
        let (mut client, session) = client_bounded(Some("é🙂x"), bounds)?;
        let end = if elided {
            elided_end(&session, "é🙂y")?
        } else {
            block_frame(&session, "ended", "é🙂y", None)
        };
        client.peer_mut().frames.push_back(end);
        client.pump()?;
        assert!(
            client.diagnostics().gaps_observed > 0,
            "matching kept text and byte count cannot prove discarded content"
        );
    }
    Ok(())
}

#[test]
fn full_end_keeps_utf8_bound_and_counts_the_complete_suffix() -> TestResult {
    let (mut client, session) = client_bounded(
        Some("é"),
        Bounds {
            max_slice_bytes: 3,
            ..Bounds::PRODUCTION
        },
    )?;
    client
        .peer_mut()
        .frames
        .push_back(block_frame(&session, "ended", "é🙂x", None));
    client.pump()?;
    assert_eq!(rendered(&client), "é");
    let slice = client.view().ok_or("view")?.slices.last().ok_or("slice")?;
    assert_eq!(slice.truncated_bytes, 5);
    assert!(slice.closed);
    assert_eq!(client.diagnostics().gaps_observed, 0);
    Ok(())
}

#[test]
fn refused_candidate_keeps_the_previous_view_and_durable_baseline() -> TestResult {
    let (mut client, session) = client(Some("é"))?;
    change_snapshot(&mut client, |value| {
        value["revision"]["seq"] = json!(900);
        let block = json!({"assistantMessageID":MESSAGE,"kind":"text","ordinal":0,"text":"candidate","bytes":9});
        value["provisional"] = json!([block.clone(), block]);
    })?;
    let before = rendered(&client);
    let failure = client
        .reconcile(&session)
        .err()
        .ok_or("overlapping provisional must fail")?;
    assert!(
        matches!(
            failure,
            grapt_client::client::ClientError::Projection(
                grapt_client::projection::ProjectionError::OverlappingOpen { .. }
            )
        ),
        "{failure}"
    );
    assert_eq!(
        rendered(&client),
        before,
        "candidate leaked before validation finished"
    );
    assert!(client.permissions().is_empty());
    assert!(client.forms().is_empty());
    client
        .peer_mut()
        .frames
        .push_back(block_frame(&session, "ended", "é🙂x", None));
    client.pump()?;
    assert_eq!(
        rendered(&client),
        "é🙂x",
        "failed candidate advanced baseline beyond retained end"
    );
    Ok(())
}

#[test]
fn contradictory_provisional_byte_count_does_not_replace_the_view() -> TestResult {
    let (mut client, session) = client(Some("é"))?;
    change_snapshot(&mut client, |value| {
        value["provisional"][0]["text"] = json!("replacement");
        value["provisional"][0]["bytes"] = json!(1);
    })?;
    assert!(client.reconcile(&session).is_err());
    assert_eq!(rendered(&client), "é");
    Ok(())
}

#[test]
fn shorter_snapshot_for_the_same_live_block_reports_visible_resync() -> TestResult {
    let (mut client, session) = client(Some("é🙂x"))?;
    change_snapshot(&mut client, |value| {
        value["provisional"][0]["text"] = json!("é");
        value["provisional"][0]["bytes"] = json!(2);
    })?;
    client.reconcile(&session)?;
    assert_eq!(rendered(&client), "é");
    assert!(
        client.gap_recovery_pending(),
        "shorter coherent install hid lost rendered bytes"
    );
    assert!(client.diagnostics().gaps_observed > 0);
    Ok(())
}

fn observation_frame(session: &SessionWireId, kind: &str, revision: u64) -> FramePoll {
    FramePoll::Positioned {
        id: format!("observation-{revision}"),
        data: json!({"id":format!("evt_observation_{revision}"),"type":"session.status","created":revision,
            "data":{"sessionID":session.spelled(),"status":{"type":"busy"},
                "state":{"type":kind,"revision":revision,"executionID":"exe_current"}}}).to_string(),
    }
}

fn observed_terminal(session: &SessionWireId, execution: &str, seq: u64) -> FramePoll {
    FramePoll::Positioned {
        id: format!("terminal-{seq}"),
        data: json!({"id":format!("evt_observed_terminal_{seq}"),"type":"session.execution.succeeded","created":seq,
            "durable":{"aggregateID":session.spelled(),"seq":seq,"version":1},
            "data":{"sessionID":session.spelled(),"executionID":execution}}).to_string(),
    }
}

#[test]
fn observation_newer_terminal_remains_live_and_survives_an_older_snapshot() -> TestResult {
    use grapt_client::projection::{ExecutionState, Outcome};
    let (mut client, session) = client(None)?;
    client
        .peer_mut()
        .frames
        .push_back(observation_frame(&session, "running", 110));
    client.pump()?;
    assert_eq!(
        client.observed_activity(&session),
        Some(grapt_client::client::SessionActivity::Running)
    );
    client
        .peer_mut()
        .frames
        .push_back(observed_terminal(&session, "exe_current", 115));
    client.pump()?;
    assert_eq!(
        client.diagnostics().gaps_observed,
        0,
        "a new terminal is evidence, not a duplicate projection defect"
    );
    change_snapshot(&mut client, |value| value["revision"]["seq"] = json!(112))?;
    client.reconcile(&session)?;
    assert!(
        matches!(&client.view().ok_or("view")?.execution, ExecutionState::Settled { execution_id: Some(id), outcome: Outcome::Succeeded } if id.spelled() == "exe_current")
    );
    assert_eq!(
        client.observed_activity(&session),
        Some(grapt_client::client::SessionActivity::Succeeded)
    );
    Ok(())
}

#[test]
fn observation_older_terminal_keeps_the_current_execution_without_a_gap() -> TestResult {
    use grapt_client::projection::ExecutionState;
    let (mut client, session) = client(None)?;
    client
        .peer_mut()
        .frames
        .push_back(observation_frame(&session, "running", 110));
    client.pump()?;
    client
        .peer_mut()
        .frames
        .push_back(observed_terminal(&session, "exe_previous", 105));
    client.pump()?;
    assert_eq!(
        client.diagnostics().gaps_observed,
        0,
        "historical terminal evidence must not be attributed to the current owner"
    );
    assert!(
        matches!(&client.view().ok_or("view")?.execution, ExecutionState::Running { execution_id } if execution_id.spelled() == "exe_current")
    );
    assert_eq!(
        client.observed_activity(&session),
        Some(grapt_client::client::SessionActivity::Running)
    );
    Ok(())
}

#[test]
fn observation_unpositioned_idle_cannot_replace_a_listed_sessions_typed_state() -> TestResult {
    let (mut client, _) = client(None)?;
    let other = client
        .peer_mut()
        .inner
        .create_session("other listed session")
        .ok_or("session")?;
    client.refresh_sessions()?;
    client
        .peer_mut()
        .frames
        .push_back(observation_frame(&other, "running", 110));
    client.pump()?;
    assert_eq!(
        client.observed_activity(&other),
        Some(grapt_client::client::SessionActivity::Running)
    );
    client.peer_mut().frames.push_back(FramePoll::Positioned {
        id: "old-idle".into(),
        data: json!({"id":"evt_unpositioned_idle","type":"session.idle","created":111,
            "data":{"sessionID":other.spelled()}})
        .to_string(),
    });
    client.pump()?;
    assert_eq!(
        client.observed_activity(&other),
        Some(grapt_client::client::SessionActivity::Running),
        "an unpositioned idle must obey the same anchor policy as state-less status"
    );
    Ok(())
}

#[test]
fn observation_snapshot_refuses_a_state_revision_beyond_its_committed_cut() -> TestResult {
    let (mut client, session) = client(None)?;
    change_snapshot(&mut client, |value| {
        value["revision"]["seq"] = json!(150);
        value["execution"]["state"] =
            json!({"type":"running","revision":200,"executionID":"exe_current"});
    })?;
    assert!(
        client.reconcile(&session).is_err(),
        "state revision 200 cannot describe a snapshot committed at 150"
    );
    assert_eq!(client.observed_activity(&session), None);
    assert_ne!(client.view().ok_or("view")?.idle, Some(false));
    Ok(())
}

#[test]
fn observation_newer_terminal_without_execution_identity_requires_recovery() -> TestResult {
    let (mut client, session) = client(None)?;
    client
        .peer_mut()
        .frames
        .push_back(observation_frame(&session, "running", 110));
    client.pump()?;
    let FramePoll::Positioned { id, data } = observed_terminal(&session, "exe_current", 115) else {
        return Err("positioned terminal fixture".into());
    };
    let mut payload: serde_json::Value = serde_json::from_str(&data)?;
    payload["data"]
        .as_object_mut()
        .ok_or("terminal object")?
        .remove("executionID");
    client.peer_mut().frames.push_back(FramePoll::Positioned {
        id,
        data: payload.to_string(),
    });
    client.pump()?;
    assert!(
        client.diagnostics().gaps_observed > 0,
        "missing identity on a newer terminal is malformed evidence, not superseded evidence"
    );
    Ok(())
}

#[test]
fn observation_unselected_terminal_keeps_execution_attribution_checks() -> TestResult {
    use grapt_client::client::SessionActivity;
    let (mut client, _) = client(None)?;
    let other = client
        .peer_mut()
        .inner
        .create_session("other execution owner")
        .ok_or("session")?;
    client.refresh_sessions()?;
    client
        .peer_mut()
        .frames
        .push_back(observation_frame(&other, "running", 110));
    client.pump()?;
    assert_eq!(
        client.observed_activity(&other),
        Some(SessionActivity::Running)
    );
    client
        .peer_mut()
        .frames
        .push_back(observed_terminal(&other, "exe_different", 115));
    client.pump()?;
    assert_ne!(
        client.observed_activity(&other),
        Some(SessionActivity::Succeeded),
        "a different execution's terminal cannot settle the listed session's current owner"
    );
    Ok(())
}

#[test]
fn observation_older_interrupt_is_retained_as_history_without_settling_the_current_owner(
) -> TestResult {
    use grapt_client::projection::ExecutionState;
    let (mut client, session) = client(None)?;
    client
        .peer_mut()
        .frames
        .push_back(observation_frame(&session, "running", 110));
    client.pump()?;
    let FramePoll::Positioned { id, data } = observed_terminal(&session, "exe_previous", 105)
    else {
        return Err("positioned terminal fixture".into());
    };
    let mut payload: serde_json::Value = serde_json::from_str(&data)?;
    payload["type"] = json!("session.execution.interrupted");
    client.peer_mut().frames.push_back(FramePoll::Positioned {
        id,
        data: payload.to_string(),
    });
    client.pump()?;
    let view = client.view().ok_or("view")?;
    assert!(
        matches!(&view.execution, ExecutionState::Running { execution_id } if execution_id.spelled() == "exe_current")
    );
    assert_eq!(client.diagnostics().gaps_observed, 0);
    assert_eq!(
        view.interrupted_observed.as_ref().map(Spelled::spelled),
        Some("exe_previous"),
        "an older interrupt remains execution-owned historical evidence"
    );
    Ok(())
}

fn push_observation(
    client: &mut Client<Peer>,
    session: &SessionWireId,
    kind: &str,
    revision: u64,
) -> TestResult {
    client
        .peer_mut()
        .frames
        .push_back(observation_frame(session, kind, revision));
    client.pump()?;
    Ok(())
}
fn snapshot_state(client: &mut Client<Peer>, p: u64, section: serde_json::Value) -> TestResult {
    change_snapshot(client, |value| {
        value["revision"]["seq"] = json!(p);
        value["execution"] = section;
    })
}

#[test]
fn observation_equal_and_older_statuses_do_not_change_selected_or_listed_presentation() -> TestResult
{
    use grapt_client::client::SessionActivity;
    let (mut client, selected) = client(None)?;
    let other = client
        .peer_mut()
        .inner
        .create_session("listed observations")
        .ok_or("session")?;
    client.refresh_sessions()?;
    for session in [&selected, &other] {
        push_observation(&mut client, session, "running", 110)?;
        push_observation(&mut client, session, "completed", 109)?;
        push_observation(&mut client, session, "completed", 110)?;
        assert_eq!(
            client.observed_activity(session),
            Some(SessionActivity::Running)
        );
        if session == &selected {
            assert_eq!(client.view().ok_or("view")?.idle, Some(false));
        }
        push_observation(&mut client, session, "completed", 111)?;
        assert_eq!(
            client.observed_activity(session),
            Some(SessionActivity::Succeeded)
        );
        if session == &selected {
            assert_eq!(client.view().ok_or("view")?.idle, Some(true));
        }
    }
    Ok(())
}

#[test]
fn observation_snapshot_forms_clear_older_pairs_and_preserve_newer_pairs_atomically() -> TestResult
{
    use grapt_client::client::SessionActivity;
    for held in [90, 110] {
        for section in [
            json!({}),
            json!({"stateError":{"code":"unreadable","message":"state unavailable"}}),
            json!({"state":{"type":"idle","revision":100}}),
        ] {
            let (mut client, session) = client_at(None, Bounds::PRODUCTION, 80)?;
            push_observation(&mut client, &session, "running", held)?;
            snapshot_state(&mut client, 100, section.clone())?;
            client.reconcile(&session)?;
            if held > 100 {
                assert_eq!(
                    client.observed_activity(&session),
                    Some(SessionActivity::Running)
                );
                assert_eq!(client.view().ok_or("view")?.idle, Some(false));
            } else if section.get("state").is_some() {
                assert_eq!(
                    client.observed_activity(&session),
                    Some(SessionActivity::Idle)
                );
                assert_eq!(client.view().ok_or("view")?.idle, Some(true));
            } else {
                assert_eq!(client.observed_activity(&session), None);
                assert_eq!(client.view().ok_or("view")?.idle, None);
                assert!(!matches!(
                    client.view().ok_or("view")?.execution,
                    grapt_client::projection::ExecutionState::Running { .. }
                ));
            }
        }
    }
    Ok(())
}

#[test]
fn observation_all_state_kinds_derive_the_current_view_and_activity() -> TestResult {
    use grapt_client::{
        client::SessionActivity as Activity,
        projection::{ExecutionState, Outcome},
    };
    for (kind, idle, activity) in [
        ("idle", Some(true), Some(Activity::Idle)),
        ("running", Some(false), Some(Activity::Running)),
        ("waiting", Some(false), Some(Activity::Running)),
        ("paused", Some(true), None),
        ("completed", Some(true), Some(Activity::Succeeded)),
        ("failed", Some(true), Some(Activity::Failed)),
        ("interrupted", Some(true), Some(Activity::Interrupted)),
        ("disconnected", None, None),
    ] {
        let (mut client, session) = client(None)?;
        push_observation(&mut client, &session, "running", 110)?;
        snapshot_state(
            &mut client,
            120,
            json!({"state":{"type":kind,"revision":120,"executionID":"exe_observed"}}),
        )?;
        client.reconcile(&session)?;
        let view = client.view().ok_or("view")?;
        assert_eq!(view.idle, idle, "{kind}");
        assert_eq!(client.observed_activity(&session), activity, "{kind}");
        match kind {
            "running" | "waiting" => assert!(
                matches!(&view.execution, ExecutionState::Running { execution_id } if execution_id.spelled()=="exe_observed")
            ),
            "completed" | "failed" | "interrupted" => {
                let expected = match kind {
                    "completed" => Outcome::Succeeded,
                    "failed" => Outcome::Failed,
                    _ => Outcome::Interrupted,
                };
                assert!(
                    matches!(&view.execution, ExecutionState::Settled { execution_id: Some(id), outcome } if id.spelled()=="exe_observed" && *outcome==expected)
                );
            }
            _ => assert!(!matches!(view.execution, ExecutionState::Running { .. })),
        }
    }
    Ok(())
}

#[test]
fn observation_contradictory_snapshot_and_malformed_state_leave_the_pair_unchanged() -> TestResult {
    use grapt_client::{client::SessionActivity, projection::ExecutionState};
    let (mut client, session) = client(None)?;
    push_observation(&mut client, &session, "running", 110)?;
    snapshot_state(
        &mut client,
        120,
        json!({"state":{"type":"idle","revision":120},"stateError":{"code":"unreadable","message":"contradictory"}}),
    )?;
    assert!(client.reconcile(&session).is_err());
    push_observation(&mut client, &session, "completed", 109)?;
    assert_eq!(
        client.observed_activity(&session),
        Some(SessionActivity::Running)
    );
    snapshot_state(&mut client, 100, json!({}))?;
    let FramePoll::Positioned { id, data } = observation_frame(&session, "completed", 111) else {
        return Err("state fixture".into());
    };
    let mut payload: serde_json::Value = serde_json::from_str(&data)?;
    payload["data"]["state"]["revision"] = json!("malformed");
    client.peer_mut().frames.push_back(FramePoll::Positioned {
        id,
        data: payload.to_string(),
    });
    client.pump()?;
    assert!(client.diagnostics().gaps_observed > 0);
    assert_eq!(
        client.observed_activity(&session),
        Some(SessionActivity::Running)
    );
    assert!(
        matches!(&client.view().ok_or("view")?.execution, ExecutionState::Running { execution_id } if execution_id.spelled()=="exe_current")
    );
    Ok(())
}

#[test]
fn observation_unselected_terminal_pair_survives_an_older_snapshot() -> TestResult {
    use grapt_client::client::SessionActivity;
    let (mut client, _) = client(None)?;
    let other = client
        .peer_mut()
        .inner
        .create_session("listed terminal")
        .ok_or("session")?;
    client.refresh_sessions()?;
    push_observation(&mut client, &other, "running", 110)?;
    client
        .peer_mut()
        .frames
        .push_back(observed_terminal(&other, "exe_current", 115));
    client.pump()?;
    assert_eq!(
        client.observed_activity(&other),
        Some(SessionActivity::Succeeded)
    );
    change_snapshot(&mut client, |value| {
        value["revision"] = json!({"aggregate":other.spelled(),"seq":112});
        value["execution"] = json!({"stateError":{"code":"unreadable","message":"older state"}});
    })?;
    client.reconcile(&other)?;
    assert_eq!(
        client.observed_activity(&other),
        Some(SessionActivity::Succeeded)
    );
    Ok(())
}
