//! After a cut, the snapshot read waits, bounded, for the next stream's first
//! frame: a positions stream resumes on a snapshot, never on a message page; a
//! stream silent past the bound is retired; and a service without positions
//! or without the snapshot is refused as too old.
use grapt_client::{
    bounds::Bounds,
    client::Connection,
    contract::Operation,
    double::ContractDouble,
    projection::ExecutionState,
    transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse},
    wire::{SessionWireId, Spelled},
    Client,
};
use serde_json::json;
use std::{collections::VecDeque, time::Duration};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const MESSAGE: &str = "msg_after_cut";

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    frames: VecDeque<FramePoll>,
    page: serde_json::Value,
    snapshot: WireResponse,
    snapshots: usize,
    page_reads: usize,
    permission_reads: usize,
    retired: usize,
    hold_snapshot: bool,
    pending_read: Option<WireResponse>,
    cancelled_reads: usize,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        match request.operation() {
            Operation::SessionSnapshot => {
                self.snapshots += 1;
                return Ok(self.snapshot.clone());
            }
            Operation::MessageList => self.page_reads += 1,
            Operation::PermissionRequestList => self.permission_reads += 1,
            _ => {}
        }
        self.inner.call(request)
    }
}
impl Connection for Peer {
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        let response = self.call(request)?;
        if self.hold_snapshot && request.operation() == Operation::SessionSnapshot {
            assert!(self.pending_read.is_none(), "one pending read lane");
            self.pending_read = Some(response);
            Ok(None)
        } else {
            Ok(Some(response))
        }
    }
    fn poll_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        Ok(if self.hold_snapshot {
            None
        } else {
            self.pending_read.take()
        })
    }
    fn cancel_read(&mut self) {
        if self.pending_read.take().is_some() {
            self.cancelled_reads += 1;
        }
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        Ok(self.frames.pop_front().unwrap_or(FramePoll::Idle))
    }
    fn resubscribe(&mut self) {}
    fn retire_stream(&mut self) {
        self.retired += 1;
        self.frames.clear();
    }
    fn subscribed(&self) -> bool {
        true
    }
}

fn connected() -> FramePoll {
    FramePoll::Positioned {
        id: "epoch-1".into(),
        data: json!({"id":"evt_cut_connected","type":"server.connected","data":{}}).to_string(),
    }
}
/// The first frame of a service that predates positions.
fn unpositioned_connected() -> FramePoll {
    FramePoll::Frame(
        json!({"id":"evt_cut_old_connected","type":"server.connected","data":{}}).to_string(),
    )
}
fn delta(session: &SessionWireId, text: &str, from: Option<u64>) -> FramePoll {
    let mut value = json!({"id":format!("evt_cut_delta_{text}"),"type":"session.text.delta",
        "created":1,"data":{"sessionID":session.spelled(),"assistantMessageID":MESSAGE,
        "ordinal":0,"delta":text}});
    if let Some(from) = from {
        value["data"]["from"] = json!(from);
    }
    FramePoll::Positioned {
        id: "epoch-2".into(),
        data: value.to_string(),
    }
}
fn observation_frame(session: &SessionWireId, revision: u64) -> FramePoll {
    FramePoll::Positioned {
        id: format!("observation-{revision}"),
        data: json!({"id":format!("evt_cut_observation_{revision}"),"type":"session.status",
            "created":revision,"data":{"sessionID":session.spelled(),"status":{"type":"busy"},
            "state":{"type":"running","revision":revision,"executionID":"exe_current"}}})
        .to_string(),
    }
}

/// The snapshot the service answers next: position `seq`, the open block's
/// text so far, and an execution section.
fn set_snapshot(
    client: &mut Client<Peer>,
    session: &SessionWireId,
    seq: u64,
    text: Option<&str>,
    execution: serde_json::Value,
) -> TestResult {
    let peer = client.peer_mut();
    let provisional: Vec<_> = text
        .into_iter()
        .map(|text| {
            json!({"assistantMessageID":MESSAGE,"kind":"text","ordinal":0,"text":text,
                "bytes":text.len()})
        })
        .collect();
    peer.snapshot = WireResponse {
        status: 200,
        body: serde_json::to_vec(&json!({
            "revision":{"aggregate":session.spelled(),"seq":seq},"rows":peer.page["data"],
            "cursor":peer.page["cursor"],"permissions":[],"forms":[],"execution":execution,
            "provisional":provisional
        }))?,
    };
    Ok(())
}

/// A client on a positions stream whose open block holds `aaaa`; with
/// `pump_first` false the session is selected before any frame is read.
fn launch(pump_first: bool) -> TestResult<(Client<Peer>, SessionWireId)> {
    let mut inner = ContractDouble::new();
    let session = inner.create_session("after cut").ok_or("session bound")?;
    let rows = inner.call(&WireRequest::new(
        Operation::MessageList,
        Some(&session),
        None,
    )?)?;
    let page: serde_json::Value = serde_json::from_slice(&rows.body)?;
    let mut client = Client::new(
        Peer {
            inner,
            frames: VecDeque::from([connected()]),
            page,
            snapshot: WireResponse {
                status: 200,
                body: Vec::new(),
            },
            snapshots: 0,
            page_reads: 0,
            permission_reads: 0,
            retired: 0,
            hold_snapshot: false,
            pending_read: None,
            cancelled_reads: 0,
        },
        Bounds::PRODUCTION,
    );
    set_snapshot(&mut client, &session, 100, Some("aaaa"), json!({}))?;
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
    if pump_first {
        client.pump_at(Duration::ZERO)?;
    }
    client.select(&session)?;
    Ok((client, session))
}

/// A launched positions client whose open block has streamed `aaaabbbb`.
fn streaming() -> TestResult<(Client<Peer>, SessionWireId)> {
    let (mut client, session) = launch(true)?;
    assert_eq!(client.peer_mut().snapshots, 1, "launch read a snapshot");
    client
        .peer_mut()
        .frames
        .push_back(delta(&session, "bbbb", Some(4)));
    client.pump_at(Duration::from_millis(500))?;
    assert_eq!(rendered(&client), "aaaabbbb");
    Ok((client, session))
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

fn at(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

/// Pump at each instant; true while nothing was read and nothing retired.
fn waits_through(client: &mut Client<Peer>, instants: &[u64]) -> TestResult<bool> {
    let peer = client.peer_mut();
    let before = (peer.snapshots, peer.page_reads, peer.retired);
    for millis in instants {
        client.pump_at(at(*millis))?;
    }
    let peer = client.peer_mut();
    Ok((peer.snapshots, peer.page_reads, peer.retired) == before)
}

/// The refusal a service too old for this client earns.
fn too_old(result: Result<bool, grapt_client::ClientError>) -> bool {
    matches!(
        &result,
        Err(grapt_client::ClientError::Transport(
            TransportError::ServerTooOld { .. }
        ))
    ) && result
        .err()
        .is_some_and(|error| error.to_string().contains("Upgrade the Grapt server"))
}

fn cut(client: &mut Client<Peer>, frame: FramePoll, millis: u64) -> TestResult {
    client.peer_mut().frames.push_back(frame);
    client.pump_at(at(millis))?;
    Ok(())
}

#[test]
fn a_cut_resumes_on_one_snapshot_and_never_reads_a_message_page() -> TestResult {
    for frame in [FramePoll::Closed, FramePoll::Gap] {
        let (mut client, session) = streaming()?;
        let gaps = client.diagnostics().gaps_observed;
        cut(&mut client, frame.clone(), 1_000)?;
        set_snapshot(&mut client, &session, 150, Some("aaaabbbbcccc"), json!({}))?;
        let peer = client.peer_mut();
        peer.frames.push_back(connected());
        peer.frames.push_back(delta(&session, "dddd", Some(12)));
        client.pump_at(at(1_100))?;
        client.pump_at(at(1_200))?;
        assert_eq!(client.peer_mut().snapshots, 2, "{frame:?}: one snapshot");
        assert_eq!(
            client.peer_mut().page_reads,
            0,
            "{frame:?}: no message page"
        );
        assert_eq!(rendered(&client), "aaaabbbbccccdddd", "{frame:?}");
        if frame == FramePoll::Closed {
            assert_eq!(client.diagnostics().gaps_observed, gaps, "no refusal");
        }
    }
    Ok(())
}

#[test]
fn an_absent_first_frame_retires_the_stream_at_the_deadline() -> TestResult {
    let (mut client, _) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    assert!(waits_through(&mut client, &[2_000, 3_000, 4_000, 4_999])?);
    assert!(
        matches!(client.recovery_wait_at(at(4_999)), Some(wait) if wait > Duration::ZERO),
        "the wait polls instead of spinning"
    );
    client.pump_at(at(5_000))?;
    let peer = client.peer_mut();
    assert_eq!(peer.retired, 1, "the silent stream is retired");
    assert_eq!((peer.snapshots, peer.page_reads), (1, 0), "nothing read");
    Ok(())
}

#[test]
fn a_late_handshake_after_the_deadline_reads_one_snapshot() -> TestResult {
    let (mut client, session) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    client.pump_at(at(5_000))?;
    assert_eq!(client.peer_mut().retired, 1);
    let gaps = client.diagnostics().gaps_observed;
    set_snapshot(&mut client, &session, 150, Some("aaaabbbbcccc"), json!({}))?;
    let peer = client.peer_mut();
    peer.frames.push_back(connected());
    peer.frames.push_back(delta(&session, "dddd", Some(12)));
    client.pump_at(at(6_000))?;
    client.pump_at(at(6_100))?;
    assert_eq!(client.peer_mut().snapshots, 2, "one snapshot re-read");
    assert_eq!(client.peer_mut().page_reads, 0);
    assert_eq!(rendered(&client), "aaaabbbbccccdddd");
    assert_eq!(client.diagnostics().gaps_observed, gaps, "no refusal");
    Ok(())
}

#[test]
fn the_deadline_is_total_across_another_cut() -> TestResult {
    let (mut client, _) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    assert!(waits_through(&mut client, &[2_000, 3_000])?);
    cut(&mut client, FramePoll::Closed, 3_500)?;
    assert!(waits_through(&mut client, &[4_000, 4_999])?);
    client.pump_at(at(5_000))?;
    assert_eq!(client.peer_mut().retired, 1);
    Ok(())
}

#[test]
fn selecting_during_the_wait_keeps_the_deadline_and_reads_nothing_early() -> TestResult {
    let (mut client, _) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    client.pump_at(at(2_000))?;
    let other = client
        .peer_mut()
        .inner
        .create_session("other")
        .ok_or("session bound")?;
    client.select(&other)?;
    assert_eq!(client.peer_mut().snapshots, 1, "no direct snapshot read");
    assert!(waits_through(&mut client, &[3_000, 4_000, 4_999])?);
    client.pump_at(at(5_000))?;
    assert_eq!(client.peer_mut().retired, 1, "the same deadline");
    Ok(())
}

#[test]
fn an_unpositioned_first_frame_after_a_cut_refuses_the_server_as_too_old() -> TestResult {
    let (mut client, _) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    let retired = client.peer_mut().retired;
    // A positioned connect later on the SAME stream cannot redeem it.
    client.peer_mut().frames.push_back(unpositioned_connected());
    client.peer_mut().frames.push_back(connected());
    assert!(too_old(client.pump_at(at(1_500))));
    assert_eq!(
        client.peer_mut().retired,
        retired + 1,
        "the stream is retired"
    );
    // The refusal is terminal: later pumps keep it and open nothing.
    for later in [1_600, 1_700, 60_000] {
        assert!(too_old(client.pump_at(at(later))));
    }
    assert_eq!(client.peer_mut().retired, retired + 1, "retired once");
    assert_eq!(client.peer_mut().page_reads, 0);
    assert_eq!(client.peer_mut().snapshots, 1, "no snapshot on its word");
    Ok(())
}

#[test]
fn launch_waits_for_the_first_frame_then_reads_one_snapshot() -> TestResult {
    let (mut client, _) = launch(false)?;
    assert_eq!(client.peer_mut().page_reads, 0, "no message page");
    assert_eq!(client.peer_mut().snapshots, 0, "no read before the stream");
    client.pump_at(at(100))?;
    assert_eq!(client.peer_mut().snapshots, 1, "one snapshot");
    assert_eq!(rendered(&client), "aaaa");
    Ok(())
}

#[test]
fn a_snapshot_404_refuses_the_server_as_too_old_on_both_reads() -> TestResult {
    // The direct read.
    let (mut client, session) = streaming()?;
    client.peer_mut().snapshot = WireResponse {
        status: 404,
        body: Vec::new(),
    };
    let snapshots = client.peer_mut().snapshots;
    assert!(too_old(client.reconcile(&session).map(|()| true)));
    assert_eq!(client.peer_mut().page_reads, 0, "no message page instead");
    stays_refused(&mut client, &session, snapshots + 1)?;
    // The queued read after a cut.
    let (mut client, session) = streaming()?;
    client.peer_mut().snapshot = WireResponse {
        status: 404,
        body: Vec::new(),
    };
    let snapshots = client.peer_mut().snapshots;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    client.peer_mut().frames.push_back(connected());
    assert!(too_old(client.pump_at(at(1_100))));
    assert_eq!(client.peer_mut().page_reads, 0, "no message page instead");
    stays_refused(&mut client, &session, snapshots + 1)?;
    Ok(())
}

/// After a terminal refusal the stream is retired, later cuts, pumps and
/// reads ask the service nothing, and recovery advertises no deadline.
fn stays_refused(
    client: &mut Client<Peer>,
    session: &SessionWireId,
    snapshots: usize,
) -> TestResult {
    let retired = client.peer_mut().retired;
    assert!(retired >= 1, "the stream is retired");
    for later in [1_200, 1_300, 60_000] {
        client.peer_mut().frames.push_back(FramePoll::Closed);
        client.peer_mut().frames.push_back(connected());
        assert!(too_old(client.pump_at(at(later))));
        assert_eq!(
            client.recovery_wait_at(at(later)),
            None,
            "no recovery deadline"
        );
    }
    assert!(too_old(client.reconcile(session).map(|()| true)));
    let peer = client.peer_mut();
    assert_eq!(peer.snapshots, snapshots, "no second snapshot request");
    assert_eq!(peer.page_reads, 0);
    Ok(())
}

#[test]
fn a_session_the_service_cannot_find_is_not_a_server_too_old() -> TestResult {
    let (mut client, session) = streaming()?;
    client.peer_mut().snapshot = WireResponse {
        status: 404,
        // The product's own not-found body, byte for byte in shape.
        body: serde_json::to_vec(&json!({"_tag":"SessionNotFoundError",
            "sessionID":session.spelled(),"message":"Session was not found"}))?,
    };
    let refusal = client.reconcile(&session);
    assert!(
        matches!(
            &refusal,
            Err(grapt_client::ClientError::Refused { status: 404, message, .. })
                if message == "Session was not found"
        ),
        "{refusal:?}"
    );
    Ok(())
}

#[test]
fn another_typed_snapshot_404_keeps_its_own_message() -> TestResult {
    let (mut client, session) = streaming()?;
    client.peer_mut().snapshot = WireResponse {
        status: 404,
        body: serde_json::to_vec(&json!({"_tag":"WorkspaceNotFoundError",
            "message":"the workspace was removed"}))?,
    };
    let refusal = client.reconcile(&session);
    assert!(
        matches!(&refusal, Err(grapt_client::ClientError::Refused { status: 404, message, .. })
            if message == "the workspace was removed"),
        "{refusal:?}"
    );
    Ok(())
}

#[test]
fn a_snapshot_after_a_cut_counts_live_output_from_zero() -> TestResult {
    let (mut client, session) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    set_snapshot(
        &mut client,
        &session,
        150,
        Some("aaaabbbbcccc"),
        json!({"state":{"type":"running","revision":150,"executionID":"exe_snapshot"}}),
    )?;
    cut(&mut client, connected(), 1_100)?;
    let view = client.view().ok_or("view")?;
    assert!(matches!(view.execution, ExecutionState::Running { .. }));
    assert_eq!(view.streamed_text_bytes(), Some(0), "no open-block seeding");
    cut(&mut client, delta(&session, "dddd", Some(12)), 1_200)?;
    assert_eq!(
        client.view().and_then(|view| view.streamed_text_bytes()),
        Some(4)
    );
    Ok(())
}

#[test]
fn a_newer_held_execution_gets_no_bytes_from_the_snapshot() -> TestResult {
    // A re-read whose cut P is older than the pair this client holds: the
    // held execution is shown, and P's open-block bytes are not its output.
    let (mut client, session) = streaming()?;
    cut(&mut client, observation_frame(&session, 160), 600)?;
    set_snapshot(
        &mut client,
        &session,
        150,
        Some("aaaabbbbcccc"),
        json!({"state":{"type":"running","revision":150,"executionID":"exe_old"}}),
    )?;
    client.reconcile(&session)?;
    let view = client.view().ok_or("view")?;
    assert!(
        matches!(&view.execution, ExecutionState::Running { execution_id } if execution_id.spelled() == "exe_current")
    );
    assert_eq!(view.streamed_text_bytes(), Some(0));
    Ok(())
}

#[test]
fn a_running_observation_after_recovery_arms_the_new_output_baseline() -> TestResult {
    let (mut client, session) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    set_snapshot(&mut client, &session, 150, Some("aaaabbbbcccc"), json!({}))?;
    cut(&mut client, connected(), 1_100)?;
    assert_eq!(
        client.view().and_then(|view| view.streamed_text_bytes()),
        None
    );
    cut(&mut client, observation_frame(&session, 160), 1_200)?;
    assert!(matches!(
        client.view().ok_or("view")?.execution,
        ExecutionState::Running { .. }
    ));
    cut(&mut client, delta(&session, "dddd", Some(12)), 1_300)?;
    assert_eq!(rendered(&client), "aaaabbbbccccdddd");
    assert_eq!(
        client.view().and_then(|view| view.streamed_text_bytes()),
        Some(4)
    );
    Ok(())
}

#[test]
fn a_pending_snapshot_is_cancelled_before_selecting_a_different_session() -> TestResult {
    let (mut client, session) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    client.peer_mut().hold_snapshot = true;
    cut(&mut client, connected(), 1_100)?;
    assert!(
        client.peer_mut().pending_read.is_some(),
        "snapshot really pending"
    );
    let other = client
        .peer_mut()
        .inner
        .create_session("new selection")
        .ok_or("session bound")?;
    set_snapshot(&mut client, &other, 200, Some("new"), json!({}))?;
    client.select(&other)?;
    assert_eq!(client.peer_mut().cancelled_reads, 1, "old reply cancelled");
    assert!(client.peer_mut().pending_read.is_none());
    client.peer_mut().hold_snapshot = false;
    cut(&mut client, delta(&other, "dddd", Some(3)), 1_200)?;
    assert_eq!(client.view().ok_or("view")?.session_id, other);
    assert_eq!(
        rendered(&client),
        "newdddd",
        "old session reply never installs"
    );
    assert_ne!(client.view().ok_or("view")?.session_id, session);
    Ok(())
}
