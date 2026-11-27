//! After a cut, the history read waits, bounded, for the next stream to prove
//! its mode: a positions stream resumes on a snapshot, never on a legacy page,
//! and no history read waits on a dead stream for longer than the bound.
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
    legacy_reads: usize,
    permission_reads: usize,
    legacy_on_refusal: bool,
    legacy_pending: bool,
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
            Operation::MessageList => self.legacy_reads += 1,
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
    fn refuse_positions(&mut self) {
        if self.legacy_on_refusal {
            self.frames.clear();
            self.frames.push_back(if self.legacy_pending {
                FramePoll::Closed
            } else {
                legacy_connected()
            });
        }
    }
    fn subscribed(&self) -> bool {
        !self.legacy_pending
    }
    fn begin_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        Ok((!self.legacy_pending).then_some(true))
    }
    fn poll_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        Ok((!self.legacy_pending).then_some(true))
    }
}

fn connected() -> FramePoll {
    FramePoll::Positioned {
        id: "epoch-1".into(),
        data: json!({"id":"evt_cut_connected","type":"server.connected","data":{}}).to_string(),
    }
}
fn legacy_connected() -> FramePoll {
    FramePoll::Frame(
        json!({"id":"evt_cut_legacy_connected","type":"server.connected","data":{}}).to_string(),
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
    let legacy = inner.call(&WireRequest::new(
        Operation::MessageList,
        Some(&session),
        None,
    )?)?;
    let page: serde_json::Value = serde_json::from_slice(&legacy.body)?;
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
            legacy_reads: 0,
            permission_reads: 0,
            legacy_on_refusal: false,
            legacy_pending: false,
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

/// Pump at each instant; true while no legacy page was read.
fn no_legacy_read_through(client: &mut Client<Peer>, instants: &[u64]) -> TestResult<bool> {
    let before = client.peer_mut().legacy_reads;
    for millis in instants {
        client.pump_at(at(*millis))?;
    }
    Ok(client.peer_mut().legacy_reads == before)
}

fn cut(client: &mut Client<Peer>, frame: FramePoll, millis: u64) -> TestResult {
    client.peer_mut().frames.push_back(frame);
    client.pump_at(at(millis))?;
    Ok(())
}

#[test]
fn a_cut_resumes_on_one_snapshot_and_never_reads_a_legacy_page() -> TestResult {
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
            client.peer_mut().legacy_reads,
            0,
            "{frame:?}: no legacy page"
        );
        assert_eq!(rendered(&client), "aaaabbbbccccdddd", "{frame:?}");
        if frame == FramePoll::Closed {
            assert_eq!(client.diagnostics().gaps_observed, gaps, "no refusal");
        }
    }
    Ok(())
}

#[test]
fn an_absent_first_frame_falls_back_to_http_at_the_deadline() -> TestResult {
    let (mut client, _) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    assert!(no_legacy_read_through(
        &mut client,
        &[2_000, 3_000, 4_000, 4_999]
    )?);
    assert!(
        matches!(client.recovery_wait_at(at(4_999)), Some(wait) if wait > Duration::ZERO),
        "the wait polls instead of spinning"
    );
    client.pump_at(at(5_000))?;
    assert_eq!(
        client.peer_mut().legacy_reads,
        1,
        "HTTP read at the deadline"
    );
    assert_eq!(client.peer_mut().snapshots, 1);
    Ok(())
}

#[test]
fn a_late_positioned_handshake_replaces_the_fallback_page_before_any_delta() -> TestResult {
    let (mut client, session) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    client.pump_at(at(5_000))?;
    assert_eq!(client.peer_mut().legacy_reads, 1);
    let gaps = client.diagnostics().gaps_observed;
    set_snapshot(&mut client, &session, 150, Some("aaaabbbbcccc"), json!({}))?;
    let peer = client.peer_mut();
    peer.frames.push_back(connected());
    peer.frames.push_back(delta(&session, "dddd", Some(12)));
    client.pump_at(at(6_000))?;
    client.pump_at(at(6_100))?;
    assert_eq!(client.peer_mut().snapshots, 2, "one snapshot re-read");
    assert_eq!(client.peer_mut().legacy_reads, 1);
    assert_eq!(rendered(&client), "aaaabbbbccccdddd");
    assert_eq!(client.diagnostics().gaps_observed, gaps, "no refusal");
    Ok(())
}

#[test]
fn the_deadline_is_total_across_another_cut() -> TestResult {
    let (mut client, _) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    assert!(no_legacy_read_through(&mut client, &[2_000, 3_000])?);
    cut(&mut client, FramePoll::Closed, 3_500)?;
    assert!(no_legacy_read_through(&mut client, &[4_000, 4_999])?);
    client.pump_at(at(5_000))?;
    assert_eq!(client.peer_mut().legacy_reads, 1);
    Ok(())
}

#[test]
fn selecting_during_the_wait_keeps_the_deadline_and_reads_no_page_early() -> TestResult {
    let (mut client, _) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    client.pump_at(at(2_000))?;
    let other = client
        .peer_mut()
        .inner
        .create_session("other")
        .ok_or("session bound")?;
    client.select(&other)?;
    assert_eq!(client.peer_mut().legacy_reads, 0, "no direct legacy read");
    assert!(no_legacy_read_through(&mut client, &[3_000, 4_000, 4_999])?);
    client.pump_at(at(5_000))?;
    assert_eq!(client.peer_mut().legacy_reads, 1, "the same deadline");
    Ok(())
}

#[test]
fn a_legacy_first_frame_after_a_cut_reads_the_page_at_once() -> TestResult {
    let (mut client, _) = streaming()?;
    cut(&mut client, FramePoll::Closed, 1_000)?;
    cut(&mut client, legacy_connected(), 1_500)?;
    assert_eq!(client.peer_mut().legacy_reads, 1);
    assert_eq!(client.peer_mut().snapshots, 1);
    Ok(())
}

#[test]
fn launch_reads_at_once_and_a_positioned_proof_replaces_that_page() -> TestResult {
    let (mut client, _) = launch(false)?;
    assert_eq!(client.peer_mut().legacy_reads, 1, "launch is unchanged");
    assert_eq!(client.peer_mut().snapshots, 0);
    client.pump_at(at(100))?;
    assert_eq!(client.peer_mut().snapshots, 1, "one snapshot re-read");
    assert_eq!(rendered(&client), "aaaa");
    Ok(())
}

/// A snapshot 404 on a positions connection: its legacy replacement may never
/// send a first frame.
fn refused(legacy_pending: bool) -> TestResult<(Client<Peer>, SessionWireId)> {
    let (mut client, session) = streaming()?;
    let peer = client.peer_mut();
    peer.snapshot.status = 404;
    peer.legacy_on_refusal = true;
    peer.legacy_pending = legacy_pending;
    Ok((client, session))
}

#[test]
fn a_404_wait_for_a_silent_legacy_stream_is_bounded() -> TestResult {
    // A positioned delta without an offset is refused; its snapshot read
    // answers 404 at 1 s, and the wait for the legacy stream starts at 2 s.
    let (mut client, session) = refused(true)?;
    cut(&mut client, delta(&session, "x", None), 1_000)?;
    assert_eq!(client.peer_mut().snapshots, 2, "the refusal was exercised");
    assert!(no_legacy_read_through(&mut client, &[2_000, 3_000])?);
    assert!(
        client.peer_mut().frames.is_empty(),
        "the retired stream closed"
    );
    cut(&mut client, FramePoll::Closed, 4_000)?;
    assert!(no_legacy_read_through(&mut client, &[5_000, 5_999])?);
    client.pump_at(at(6_000))?;
    assert_eq!(
        client.peer_mut().legacy_reads,
        1,
        "HTTP read at the deadline"
    );
    Ok(())
}

#[test]
fn a_404_legacy_first_frame_reads_the_page_at_once() -> TestResult {
    let (mut client, session) = refused(false)?;
    client.reconcile(&session)?;
    client.pump_at(at(1_000))?;
    client.pump_at(at(1_500))?;
    assert_eq!(client.peer_mut().legacy_reads, 1);
    Ok(())
}

#[test]
fn a_404_wait_reads_permissions_and_keeps_its_queued_read() -> TestResult {
    let (mut client, session) = refused(true)?;
    let permissions = client.peer_mut().permission_reads;
    client.reconcile(&session)?;
    client.pump_at(at(1_000))?;
    assert_eq!(client.peer_mut().permission_reads, permissions + 1);
    assert!(no_legacy_read_through(&mut client, &[2_000, 3_000, 4_999])?);
    assert_eq!(
        client.peer_mut().permission_reads,
        permissions + 1,
        "recovery did not churn the queued read"
    );
    client.pump_at(at(5_000))?;
    assert_eq!(
        client.peer_mut().legacy_reads,
        1,
        "the queued read survived"
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
