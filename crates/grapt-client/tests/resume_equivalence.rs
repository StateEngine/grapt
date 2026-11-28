//! A certified replay has the uninterrupted client's model and read trace.
use grapt_client::{
    bounds::Bounds,
    client::Connection,
    contract::Operation,
    double::ContractDouble,
    transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse},
    wire::{self, SessionWireId, Spelled},
    Client,
};
use grapt_client_transport::connection::{ResumeToken, StreamBinding};
use serde_json::{json, Value};
use std::{collections::VecDeque, time::Duration};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const MESSAGE: &str = "msg_resume";

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    frames: VecDeque<FramePoll>,
    open: bool,
    watched: Option<grapt_types::SessionId>,
    credential: u64,
    offered: Vec<Option<ResumeToken>>,
    ops: Vec<Operation>,
    all_ops: Vec<Operation>,
    answers: Vec<bool>,
    retired: usize,
    held_read: Option<WireResponse>,
    hold_directory: bool,
    page: WireResponse,
    snapshot: WireResponse,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        self.all_ops.push(request.operation());
        match request.operation() {
            Operation::PermissionRequestList
            | Operation::SessionSnapshot
            | Operation::MessageList => {
                self.ops.push(request.operation());
            }
            _ => {}
        }
        match request.operation() {
            Operation::SessionSnapshot => Ok(self.snapshot.clone()),
            Operation::MessageList => Ok(self.page.clone()),
            _ => self.inner.call(request),
        }
    }
}
impl Connection for Peer {
    fn begin_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        self.cancel_read();
        self.resubscribe();
        Ok(Some(self.open))
    }

    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        let response = self.call(request)?;
        if self.hold_directory && request.operation() == Operation::ProjectCurrent {
            assert!(self.held_read.is_none());
            self.held_read = Some(response);
            Ok(None)
        } else {
            Ok(Some(response))
        }
    }
    fn poll_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        Ok(if self.hold_directory {
            None
        } else {
            self.held_read.take()
        })
    }
    fn cancel_read(&mut self) {
        self.held_read = None;
    }
    fn resume_answered(&mut self, certified: bool) {
        self.answers.push(certified);
    }

    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        if !self.open {
            return Ok(FramePoll::Idle);
        }
        let frame = self.frames.pop_front().unwrap_or(FramePoll::Idle);
        if matches!(frame, FramePoll::Closed | FramePoll::Gap) {
            self.open = false;
        }
        Ok(frame)
    }
    fn resubscribe(&mut self) {
        self.open = true;
    }
    fn subscribed(&self) -> bool {
        self.open
    }
    fn retire_stream(&mut self) {
        self.retired += 1;
        self.frames.clear();
        self.open = false;
    }
    fn watch_session(&mut self, session: Option<grapt_types::SessionId>) {
        self.watched = session;
    }
    fn stream_binding(&self) -> Option<StreamBinding> {
        self.open
            .then(|| StreamBinding::new("http://127.0.0.1", self.credential, self.watched.clone()))
    }
    fn resume_from(&mut self, token: Option<ResumeToken>) {
        self.offered.push(token);
    }
}
fn positioned(id: &str, value: Value) -> FramePoll {
    FramePoll::Positioned {
        id: id.into(),
        data: value.to_string(),
    }
}
fn connected(id: &str) -> FramePoll {
    positioned(
        id,
        json!({"id":"evt_resume_connected","type":"server.connected","data":{}}),
    )
}
fn status(session: &SessionWireId, revision: u64) -> FramePoll {
    positioned(
        &format!("position-{revision}"),
        json!({"id":format!("evt_status_{revision}"),
        "type":"session.status","created":revision,"data":{"sessionID":session.spelled(),
        "status":{"type":"busy"},"state":{"type":"running","revision":revision,"executionID":"exe_resume"}}}),
    )
}
fn script(session: &SessionWireId) -> Vec<FramePoll> {
    vec![
        status(session, 160),
        positioned(
            "position-text",
            json!({"id":"evt_resume_text","type":"session.text.delta","created":161,
            "data":{"sessionID":session.spelled(),"assistantMessageID":MESSAGE,"ordinal":0,"delta":"bbbb","from":4}}),
        ),
        positioned(
            "position-hint",
            json!({"id":"evt_resume_tool","type":"session.tool.called","created":165,
            "durable":{"aggregateID":session.spelled(),"seq":165,"version":1},
            "data":{"sessionID":session.spelled(),"callID":"call_resume","assistantMessageID":MESSAGE}}),
        ),
        status(session, 170),
        positioned(
            "position-attention",
            json!({"id":"evt_resume_attention","type":"session.mail.attention","created":180,
            "data":{"sessionID":session.spelled(),"status":{"type":"unavailable"}}}),
        ),
        positioned(
            "position-terminal",
            json!({"id":"evt_resume_terminal","type":"session.execution.succeeded","created":200,
            "durable":{"aggregateID":session.spelled(),"seq":200,"version":1},
            "data":{"sessionID":session.spelled(),"executionID":"exe_resume"}}),
        ),
    ]
}
fn set_state(
    client: &mut Client<Peer>,
    session: &SessionWireId,
    revision: u64,
    tool: bool,
    done: bool,
) -> TestResult {
    let peer = client.peer_mut();
    if tool {
        let item = wire::DurableTool {
            duration_ms: None,
            id: wire::tool_call_id("call_resume")?,
            name: "execute".into(),
            state: if done {
                wire::DurableToolState::Completed {
                    input: "{}".into(),
                    output: "done".into(),
                }
            } else {
                wire::DurableToolState::Running { input: "{}".into() }
            },
            progress: vec![],
        };
        peer.page = WireResponse {
            status: 200,
            body: wire::encode_messages(
                vec![wire::assistant_message(
                    MESSAGE,
                    &[
                        wire::MessageItem::Text("aaaabbbb".into()),
                        wire::MessageItem::Tool(item),
                    ],
                    done.then_some(wire::DurableFinish::Stop),
                    None,
                    None,
                )?],
                None,
            )?,
        };
    }
    let page: grapt_api::u3a::SessionMessagesResponseDto =
        grapt_api::u3a::decode_page(&peer.page.body, wire::limits()?)?;
    #[derive(serde::Serialize)]
    struct Snapshot<'a, R, C> {
        revision: Value,
        rows: &'a R,
        cursor: &'a C,
        permissions: Vec<Value>,
        forms: Vec<Value>,
        execution: Value,
        provisional: Vec<Value>,
    }
    let provisional = if done {
        vec![]
    } else {
        let text = if tool { "aaaabbbb" } else { "aaaa" };
        vec![
            json!({"assistantMessageID":MESSAGE,"kind":"text","ordinal":0,"text":text,"bytes":text.len()}),
        ]
    };
    let execution = if revision < 160 {
        json!({})
    } else {
        json!({"state":{"type":if done {"completed"} else {"running"},"revision":revision,"executionID":"exe_resume"}})
    };
    peer.snapshot = WireResponse {
        status: 200,
        body: serde_json::to_vec(&Snapshot {
            revision: json!({"aggregate":session.spelled(),"seq":revision}),
            rows: page.data().get(),
            cursor: page.cursor().get(),
            permissions: vec![],
            forms: vec![],
            execution,
            provisional,
        })?,
    };
    Ok(())
}
fn pump(client: &mut Client<Peer>, millis: u64) -> TestResult {
    client.take_location_form_refresh();
    client.take_session_form_refresh();
    client
        .pump_at(Duration::from_millis(millis))
        .map_err(|error| format!("pump at {millis}: {error}"))?;
    Ok(())
}
fn launch() -> TestResult<(Client<Peer>, SessionWireId)> {
    let mut inner = ContractDouble::new();
    let session = inner.create_session("resume").ok_or("session bound")?;
    let page = inner.call(&WireRequest::new(
        Operation::MessageList,
        Some(&session),
        None,
    )?)?;
    let mut client = Client::new(
        Peer {
            inner,
            frames: VecDeque::from([connected("position-start")]),
            open: true,
            watched: None,
            credential: 1,
            offered: vec![],
            ops: vec![],
            all_ops: vec![],
            answers: vec![],
            retired: 0,
            held_read: None,
            hold_directory: false,
            page,
            snapshot: WireResponse {
                status: 200,
                body: vec![],
            },
        },
        Bounds::PRODUCTION,
    );
    set_state(&mut client, &session, 100, false, false)?;
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project")?;
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client
        .models()
        .iter()
        .find(|row| row.enabled)
        .cloned()
        .ok_or("model")?;
    client.select_model(&model)?;
    pump(&mut client, 0)?;
    client.select(&session)?;
    for n in 1..5 {
        pump(&mut client, n)?;
    }
    client.peer_mut().ops.clear();
    client.peer_mut().offered.clear();
    Ok((client, session))
}
fn assert_same(
    control: &Client<Peer>,
    other: &Client<Peer>,
    session: &SessionWireId,
) -> TestResult {
    let a = control.view_of(session).ok_or("control view")?;
    let b = other.view_of(session).ok_or("other view")?;
    assert_eq!(a.slices, b.slices, "transcript");
    assert_eq!(a.execution, b.execution, "execution");
    assert_eq!(a.idle, b.idle, "idle");
    assert_eq!(a.idle_created, b.idle_created, "idle stamp");
    assert_eq!(a.mail_attention, b.mail_attention, "mail attention");
    assert_eq!(a.continuity_attention, b.continuity_attention, "continuity");
    assert_eq!(
        a.streamed_text_bytes(),
        b.streamed_text_bytes(),
        "streamed bytes"
    );
    assert_eq!(
        control.observed_activity(session),
        other.observed_activity(session),
        "activity"
    );
    assert_eq!(
        control.stream_state(),
        other.stream_state(),
        "stream health"
    );
    Ok(())
}
fn assert_fresh_recovery_reads(
    control: &mut Client<Peer>,
    refused: &mut Client<Peer>,
    cut_after: usize,
) {
    for operation in [
        Operation::PermissionRequestList,
        Operation::SessionSnapshot,
        Operation::MessageList,
    ] {
        let ordinary = control
            .peer_mut()
            .ops
            .iter()
            .filter(|op| **op == operation)
            .count();
        let fresh = refused
            .peer_mut()
            .ops
            .iter()
            .filter(|op| **op == operation)
            .count();
        let extra = usize::from(operation != Operation::MessageList);
        // A fresh snapshot after the hint already holds its tool row.
        let superseded = usize::from(cut_after == 4 && operation == Operation::MessageList);
        assert_eq!(
            fresh,
            ordinary + extra - superseded,
            "cut {cut_after}: refusal adds one recovery, operation {operation:?}"
        );
    }
}
#[test]
fn certified_replay_matches_the_uncut_projection_and_reads() -> TestResult {
    for cut_after in [1, 4] {
        let (mut control, session) = launch()?;
        let (mut resumed, other) = launch()?;
        let (mut refused, _) = launch()?;
        assert_eq!(session, other);
        let frames = script(&session);
        for client in [&mut control, &mut resumed, &mut refused] {
            client.peer_mut().frames.extend(frames[..1].iter().cloned());
            pump(client, 100)?;
            if cut_after == 4 {
                client.peer_mut().frames.push_back(frames[1].clone());
                pump(client, 110)?;
                set_state(client, &session, 170, true, false)?;
                client
                    .peer_mut()
                    .frames
                    .extend(frames[2..4].iter().cloned());
            }
        }
        set_state(
            &mut refused,
            &session,
            if cut_after == 1 { 160 } else { 170 },
            cut_after == 4,
            false,
        )?;
        refused.peer_mut().frames.push_back(FramePoll::Closed);
        pump(&mut refused, 120)?;
        pump(&mut refused, 130)?;
        refused
            .peer_mut()
            .frames
            .push_back(connected("position-fresh"));
        pump(&mut refused, 140)?;
        pump(&mut refused, 145)?;
        set_state(&mut refused, &session, 170, true, false)?;
        refused
            .peer_mut()
            .frames
            .extend(frames[cut_after..5].iter().cloned());
        pump(&mut refused, 150)?;
        pump(&mut control, 120)?;
        resumed.peer_mut().frames.push_back(FramePoll::Closed);
        pump(&mut resumed, 120)?;
        let frontier = if cut_after == 1 {
            "position-160"
        } else {
            "position-text"
        };
        pump(&mut resumed, 130)?;
        assert!(
            resumed
                .peer_mut()
                .offered
                .iter()
                .any(|token| token.as_ref().is_some_and(|token| token.id == frontier)),
            "a committed pre-cut frontier must be offered"
        );
        resumed.peer_mut().frames.push_back(FramePoll::Closed);
        pump(&mut resumed, 140)?;
        resumed.peer_mut().frames.push_back(connected(frontier));
        resumed.peer_mut().frames.extend(
            frames[if cut_after == 1 { 1 } else { 2 }..5]
                .iter()
                .cloned(),
        );
        control
            .peer_mut()
            .frames
            .extend(frames[cut_after..5].iter().cloned());
        if cut_after == 1 {
            set_state(&mut control, &session, 170, true, false)?;
            set_state(&mut resumed, &session, 170, true, false)?;
        }
        pump(&mut control, 150)?;
        pump(&mut resumed, 150)?;
        assert_same(&control, &resumed, &session)?;
        assert_eq!(
            resumed.peer_mut().ops,
            control.peer_mut().ops,
            "certified replay must not add reads"
        );
        for client in [&mut control, &mut resumed, &mut refused] {
            set_state(client, &session, 200, true, true)?;
            client.peer_mut().frames.push_back(frames[5].clone());
            pump(client, 200)?;
            pump(client, 210)?;
        }
        assert_same(&control, &resumed, &session)?;
        assert_eq!(
            resumed.peer_mut().ops,
            control.peer_mut().ops,
            "terminal convergence must preserve the read trace"
        );
        assert_same(&control, &refused, &session)?;
        assert_fresh_recovery_reads(&mut control, &mut refused, cut_after);
    }
    Ok(())
}
#[test]
fn an_empty_certified_replay_restores_live_facts() -> TestResult {
    let (mut control, session) = launch()?;
    let (mut resumed, _) = launch()?;
    for client in [&mut control, &mut resumed] {
        client.peer_mut().frames.push_back(status(&session, 160));
        client
            .peer_mut()
            .frames
            .push_back(script(&session)[4].clone());
        pump(client, 100)?;
    }
    assert!(
        control
            .view_of(&session)
            .ok_or("view")?
            .mail_attention
            .is_some(),
        "attention frame is meaningful"
    );
    resumed.peer_mut().frames.push_back(FramePoll::Closed);
    pump(&mut resumed, 120)?;
    pump(&mut resumed, 130)?;
    resumed
        .peer_mut()
        .frames
        .push_back(connected("position-attention"));
    pump(&mut resumed, 140)?;
    assert_same(&control, &resumed, &session)
}
#[test]
fn an_uncertified_reconnect_reads_a_fresh_baseline() -> TestResult {
    for refusal in [
        "fresh",
        "legacy",
        "non_connected",
        "malformed",
        "credential",
    ] {
        let (mut control, session) = launch()?;
        let (mut refused, _) = launch()?;
        for client in [&mut control, &mut refused] {
            client.peer_mut().frames.push_back(status(&session, 160));
            pump(client, 100)?;
            set_state(client, &session, 160, false, false)?;
        }
        refused.peer_mut().frames.push_back(FramePoll::Closed);
        pump(&mut refused, 120)?;
        pump(&mut refused, 130)?;
        let first = match refusal {
            "fresh" => connected("position-fresh"),
            "legacy" => FramePoll::Frame(
                json!({"id":"evt_legacy","type":"server.connected","data":{}}).to_string(),
            ),
            "non_connected" => positioned(
                "position-160",
                json!({"id":"evt_other","type":"unknown","data":{}}),
            ),
            "malformed" => FramePoll::Positioned {
                id: "position-160".into(),
                data: "{".into(),
            },
            _ => {
                refused.peer_mut().credential = 2;
                connected("position-160")
            }
        };
        refused.peer_mut().frames.push_back(first);
        if refusal == "legacy" {
            // A service that predates positions is refused, never resumed.
            let refusal = refused.pump_at(Duration::from_millis(140));
            assert!(
                matches!(
                    refusal,
                    Err(grapt_client::ClientError::Transport(
                        TransportError::ServerTooOld { .. }
                    ))
                ),
                "{refusal:?}"
            );
            continue;
        }
        pump(&mut refused, 140)?;
        pump(&mut refused, 150)?;
        // A stream that proved nothing gets no snapshot: only the permission
        // list is read until a positioned connect arrives.
        let expected = if ["non_connected", "malformed"].contains(&refusal) {
            vec![Operation::PermissionRequestList]
        } else {
            vec![Operation::PermissionRequestList, Operation::SessionSnapshot]
        };
        assert_eq!(
            refused.peer_mut().ops,
            expected,
            "{refusal}: refusal owns the recovery read"
        );
        assert!(control.peer_mut().ops.is_empty());
    }
    Ok(())
}
#[test]
fn a_certificate_at_the_original_deadline_is_retired() -> TestResult {
    let (mut client, session) = launch()?;
    client.peer_mut().frames.push_back(status(&session, 160));
    pump(&mut client, 100)?;
    client.peer_mut().frames.push_back(FramePoll::Closed);
    pump(&mut client, 1_000)?;
    pump(&mut client, 1_010)?;
    client.peer_mut().frames.push_back(FramePoll::Closed);
    pump(&mut client, 2_000)?;
    pump(&mut client, 4_999)?;
    assert!(
        client.peer_mut().ops.is_empty(),
        "no history read before the total deadline"
    );
    client
        .peer_mut()
        .frames
        .push_back(connected("position-160"));
    pump(&mut client, 5_000)?;
    assert_eq!(
        client.peer_mut().ops,
        vec![Operation::PermissionRequestList]
    );
    assert!(
        client
            .peer_mut()
            .offered
            .last()
            .is_some_and(Option::is_none),
        "expired offers are withdrawn"
    );
    client
        .peer_mut()
        .frames
        .push_back(connected("position-late"));
    pump(&mut client, 5_010)?;
    pump(&mut client, 5_020)?;
    assert_eq!(
        client.peer_mut().ops,
        vec![Operation::PermissionRequestList, Operation::SessionSnapshot]
    );
    Ok(())
}

#[test]
fn an_empty_replay_keeps_all_listed_observations_beyond_hydration_cap() -> TestResult {
    use grapt_client::client::SessionActivity;
    let (mut control, selected) = launch()?;
    let (mut resumed, _) = launch()?;
    let mut sessions = vec![selected];
    for index in 1..33 {
        let a = control
            .peer_mut()
            .inner
            .create_session(&format!("session {index}"))
            .ok_or("session bound")?;
        let b = resumed
            .peer_mut()
            .inner
            .create_session(&format!("session {index}"))
            .ok_or("session bound")?;
        assert_eq!(a, b);
        sessions.push(a);
    }
    for client in [&mut control, &mut resumed] {
        client.refresh_sessions()?;
        for session in &sessions {
            client.peer_mut().frames.push_back(status(session, 160));
        }
        pump(client, 100)?;
        for session in &sessions {
            assert_eq!(
                client.observed_activity(session),
                Some(SessionActivity::Running)
            );
        }
    }
    resumed.peer_mut().frames.push_back(FramePoll::Closed);
    pump(&mut resumed, 120)?;
    for session in &sessions {
        assert_eq!(resumed.observed_activity(session), None);
    }
    resumed
        .peer_mut()
        .frames
        .push_back(connected("position-160"));
    pump(&mut resumed, 130)?;
    for session in &sessions {
        assert_eq!(
            resumed.observed_activity(session),
            control.observed_activity(session)
        );
    }
    // Equal or older replay must leave every restored revision pair in control.
    for session in &sessions {
        let mut older = match status(session, 159) {
            FramePoll::Positioned { data, .. } => serde_json::from_str::<Value>(&data)?,
            _ => return Err("positioned status".into()),
        };
        older["data"]["state"]["type"] = json!("completed");
        resumed
            .peer_mut()
            .frames
            .push_back(positioned("position-older", older));
    }
    pump(&mut resumed, 140)?;
    for session in &sessions {
        assert_eq!(
            resumed.observed_activity(session),
            Some(SessionActivity::Running)
        );
    }
    assert!(
        resumed.peer_mut().ops.is_empty(),
        "restoring all listed pairs requires no hydration"
    );
    Ok(())
}

#[test]
fn completed_hint_is_not_read_again_while_another_debt_freezes_the_frontier() -> TestResult {
    let (mut control, session) = launch()?;
    let (mut resumed, _) = launch()?;
    let script = script(&session);
    let form = positioned(
        "position-form",
        json!({"id":"evt_form_cancelled","type":"form.cancelled","created":166,
        "durable":{"aggregateID":session.spelled(),"seq":166,"version":1},
        "data":{"sessionID":session.spelled(),"id":"frm_resume"}}),
    );
    for client in [&mut control, &mut resumed] {
        client.peer_mut().frames.push_back(status(&session, 160));
        pump(client, 100)?;
        set_state(client, &session, 170, true, false)?;
        client
            .peer_mut()
            .frames
            .extend([script[2].clone(), form.clone(), script[3].clone()]);
        client.pump_at(Duration::from_millis(110))?;
        assert_eq!(
            client.peer_mut().ops,
            vec![Operation::MessageList],
            "the hint read completed"
        );
        assert!(
            client.take_session_form_refresh(),
            "an independent form refresh is outstanding"
        );
        client.refresh_forms()?;
    }
    resumed.peer_mut().frames.push_back(FramePoll::Closed);
    pump(&mut resumed, 120)?;
    pump(&mut resumed, 130)?;
    resumed
        .peer_mut()
        .frames
        .push_back(connected("position-160"));
    resumed
        .peer_mut()
        .frames
        .extend([script[2].clone(), form, script[3].clone()]);
    pump(&mut resumed, 140)?;
    assert_eq!(
        resumed.peer_mut().ops,
        control.peer_mut().ops,
        "certification must not repeat a hint read that completed before the cut"
    );
    Ok(())
}

#[test]
fn queued_catalog_is_held_until_the_resume_answer_and_runs_once() -> TestResult {
    for certified in [true, false] {
        let (mut client, session) = launch()?;
        client.peer_mut().frames.push_back(status(&session, 160));
        pump(&mut client, 100)?;
        let frontier = client.resume_frontier().ok_or("frontier")?.to_owned();
        client.enable_commands();
        client.peer_mut().all_ops.clear();
        client.peer_mut().frames.push_back(FramePoll::Closed);
        pump(&mut client, 120)?;
        pump(&mut client, 130)?;
        assert!(client.resume_pending());
        assert!(!client
            .peer_mut()
            .all_ops
            .contains(&Operation::CommandInspect));
        let answer = if certified {
            &frontier
        } else {
            "position-fresh"
        };
        client.peer_mut().frames.push_back(connected(answer));
        pump(&mut client, 140)?;
        pump(&mut client, 150)?;
        assert!(!client.resume_pending());
        assert_eq!(client.peer_mut().answers, [certified]);
        assert_eq!(
            client
                .peer_mut()
                .all_ops
                .iter()
                .filter(|op| **op == Operation::CommandInspect)
                .count(),
            1
        );
        if certified {
            assert!(
                client.peer_mut().ops.is_empty(),
                "no fresh baseline on certification"
            );
        } else {
            assert_eq!(
                client.peer_mut().ops,
                [Operation::PermissionRequestList, Operation::SessionSnapshot]
            );
        }
    }
    Ok(())
}

#[test]
fn an_inflight_directory_acquisition_refuses_resume_before_reconnect() -> TestResult {
    let (mut client, session) = launch()?;
    client.peer_mut().frames.push_back(status(&session, 160));
    pump(&mut client, 100)?;
    assert!(client.resume_frontier().is_some());
    let directory = client.projects().first().ok_or("project")?.worktree.clone();
    client.peer_mut().hold_directory = true;
    assert!(!client.advance_directory_offer(&directory, false)?);
    client.peer_mut().offered.clear();
    client.peer_mut().frames.push_back(FramePoll::Closed);
    pump(&mut client, 120)?;
    assert!(!client.resume_pending());
    assert!(client.resume_frontier().is_none());
    assert!(!client.peer_mut().offered.is_empty());
    assert!(client.peer_mut().offered.iter().all(Option::is_none));
    assert_eq!(client.peer_mut().answers, [false]);
    Ok(())
}

#[test]
fn a_host_read_retires_an_installed_unanswered_certificate_once() -> TestResult {
    let (mut client, session) = launch()?;
    client.peer_mut().frames.push_back(status(&session, 160));
    pump(&mut client, 100)?;
    let frontier = client.resume_frontier().ok_or("frontier")?.to_owned();
    client.peer_mut().frames.push_back(FramePoll::Closed);
    pump(&mut client, 120)?;
    assert!(client.resume_pending());
    client.peer_mut().frames.push_back(connected(&frontier));
    client.refresh_projects()?;
    assert!(!client.resume_pending());
    assert!(client.resume_frontier().is_none());
    assert_eq!(client.peer_mut().retired, 1);
    assert_eq!(client.peer_mut().answers, [false]);
    client.refresh_projects()?;
    assert_eq!(client.peer_mut().retired, 1);
    assert_eq!(client.peer_mut().answers, [false]);
    pump(&mut client, 130)?;
    assert!(client
        .peer_mut()
        .ops
        .contains(&Operation::PermissionRequestList));
    Ok(())
}
