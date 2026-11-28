mod ledger;
use super::*;
mod boundary;
mod boundary_fixture;
mod command_lane;
mod periodic;
mod read_gate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::{
    Connection, ContractDouble, FramePoll, Operation, Transport, TransportError, WireRequest,
    WireResponse,
};
use std::sync::mpsc;
use std::time::Duration;
mod activity_order;
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    frame: Option<FramePoll>,
    hold: bool,
    fail_history: bool,
    keep_sessions: bool,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if self.fail_history
            && matches!(
                request.operation(),
                Operation::MessageList | Operation::SessionSnapshot
            )
        {
            return Ok(WireResponse {
                status: 503,
                body: Vec::new(),
            });
        }
        let mut response = self.inner.call(request)?;
        if !self.keep_sessions && request.operation() == Operation::SessionList {
            response.body = grapt_client::wire::encode_sessions(Vec::new())
                .map_err(|_| TransportError::NotConnected)?;
        }
        Ok(response)
    }
}
impl Connection for Peer {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        // The double's own stream opens with its positioned connect first.
        if !self.inner.subscribed() {
            return self.inner.poll_frame();
        }
        if let Some(connect) = self.inner.connect() {
            return Ok(connect);
        }
        if let Some(frame) = self.frame.take() {
            return Ok(self.inner.serve(frame));
        }
        if self.hold {
            return Ok(FramePoll::Idle);
        }
        let frame = self.inner.poll_frame()?;
        if matches!(&frame, FramePoll::Frame(text) | FramePoll::Positioned { data: text, .. }
            if text.contains("session.execution.started"))
        {
            self.hold = true;
        }
        Ok(frame)
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn selected_activity_derivation_routes_escape_without_touching_a_hidden_session_draft() -> TestResult
{
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            frame: None,
            hold: false,
            fail_history: false,
            keep_sessions: false,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().ok_or("project")?.clone();
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client.models().first().ok_or("model")?.clone();
    client.select_model(&model)?;
    let session = client.create_session()?;
    assert!(
        client.sessions().is_empty(),
        "selection is outside picker page"
    );
    assert_eq!(client.selected(), Some(&session));
    assert_eq!(selected_activity(&client), None, "cold idle is unreported");
    assert!(
        !selected_running(&client),
        "absent idle falls back to idle execution"
    );
    // The stream proves itself and the session's snapshot is read before the
    // turn: it reports the new session idle.
    client.pump()?;
    assert_eq!(client.view().ok_or("view")?.idle, Some(true));
    assert_eq!(selected_activity(&client), Some(false));

    client.submit_prompt("hold this execution")?;
    client.pump()?;
    assert!(matches!(
        client.view().ok_or("view")?.execution,
        grapt_client::ExecutionState::Running { .. }
    ));
    assert_eq!(selected_activity(&client), Some(true));

    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands,
        updates,
        wake,
    };
    let mut ui = crate::ui::Ui::new(false);
    ui.composer.insert_str("KEEP pending edit");
    ui.pending = Some("uncertain original".into());
    let grapt_client::ExecutionState::Running { execution_id } =
        client.view().ok_or("view")?.execution.clone()
    else {
        return Err("running execution".into());
    };
    // A known idle fact must override the still-running transcript, and a
    // later busy fact must restore interrupt routing, from the same Client.
    // Each carries the newer typed state the server reports beside it.
    for (created, kind, idle, state) in [
        (
            1_791_695_003_000_u64,
            "idle",
            true,
            serde_json::json!({"type": "idle", "revision": 1000}),
        ),
        (
            1_791_695_004_000,
            "busy",
            false,
            serde_json::json!({"type": "running", "revision": 1001, "executionID": execution_id.spelled()}),
        ),
    ] {
        let frame = FramePoll::Frame(
            serde_json::json!({
                "id": format!("evt_status_{created}"), "type": "session.status", "created": created,
                "data": { "sessionID": session.spelled(), "status": { "type": kind }, "state": state }
            })
            .to_string(),
        );
        let frame = client.peer_mut().inner.serve(frame);
        client.peer_mut().frame = Some(frame);
        client.pump()?;
        assert_eq!(client.view().ok_or("view")?.idle, Some(idle));
        assert_eq!(selected_running(&client), !idle);
        assert_eq!(selected_activity(&client), Some(!idle));
        ui.session_update(client.selected().cloned(), selected_running(&client));
        ui.picker = None;
        crate::keys::handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        if idle {
            assert!(ui.picker.is_some(), "idle Escape opens sessions");
            assert!(matches!(
                received.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
        } else {
            assert!(ui.picker.is_none());
            assert!(matches!(
                received.try_recv()?,
                Command::InterruptAndResume(_)
            ));
        }
        assert_eq!(crate::keys::draft(&ui), "KEEP pending edit");
        assert_eq!(ui.pending.as_deref(), Some("uncertain original"));
    }
    assert_eq!(
        crate::quit::key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &mut ui,
            &backend,
            Duration::ZERO,
        ),
        Some(false)
    );
    assert!(matches!(received.try_recv()?, Command::Interrupt(_)));
    // A disconnected stream clears its live idle fact. Failed hydration
    // leaves the running transcript as the fallback until reads recover.
    client.peer_mut().fail_history = true;
    client.peer_mut().frame = Some(FramePoll::Closed);
    client.pump()?;
    assert_eq!(client.view().ok_or("view")?.idle, None);
    assert!(
        selected_running(&client),
        "absent idle falls back to running execution"
    );
    ui.session_update(client.selected().cloned(), selected_running(&client));
    crate::keys::handle_key(
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    assert!(matches!(
        received.try_recv()?,
        Command::InterruptAndResume(_)
    ));
    assert_eq!(crate::keys::draft(&ui), "KEEP pending edit");
    Ok(())
}

#[test]
fn the_earliest_outstanding_timer_becomes_the_loop_deadline() {
    assert_eq!(wake_delay([None, None, None, None]), None);
    assert_eq!(
        wake_delay([Some(Duration::from_millis(500)), None, None, None]),
        Some(Duration::from_millis(500))
    );
    assert_eq!(
        wake_delay([
            Some(Duration::from_millis(500)),
            None,
            Some(Duration::from_millis(10)),
            None
        ]),
        Some(Duration::from_millis(10)),
        "the loop must not wait past an earlier timer"
    );
}

#[test]
fn a_quiet_client_blocks_the_loop_until_a_wake_arrives() -> Result<(), String> {
    let (wake, ready) = mpsc::sync_channel(1);
    let (done, observed) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = wait(&ready, [None, None, None, None]).map_err(|e| e.to_string());
        let _ = done.send(result);
    });
    // A quiet loop must still hold its receive when the wake is sent. If
    // it has already returned, the loop polls instead of sleeping.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        observed.try_recv().is_err(),
        "a quiet client must not return before a wake"
    );
    wake.send(()).map_err(|e| e.to_string())?;
    let result: Result<(), String> = observed
        .recv_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    worker
        .join()
        .map_err(|_| "wait worker panicked".to_string())?;
    result
}

#[test]
fn a_wake_before_the_deadline_ends_the_wait_early() -> Result<(), String> {
    let (wake, ready) = mpsc::sync_channel(1);
    let (done, observed) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = wait(&ready, [Some(Duration::from_secs(30)), None, None, None])
            .map_err(|e| e.to_string());
        let _ = done.send(result);
    });
    std::thread::sleep(Duration::from_millis(20));
    wake.send(()).map_err(|e| e.to_string())?;
    let result: Result<(), String> = observed
        .recv_timeout(Duration::from_secs(5))
        .map_err(|e| e.to_string())?;
    worker
        .join()
        .map_err(|_| "wait worker panicked".to_string())?;
    result
}

#[test]
fn the_earliest_deadline_ends_the_wait_without_a_wake() -> Result<(), String> {
    let (_wake, ready) = mpsc::sync_channel(1);
    let started = tokio::time::Instant::now();
    wait(&ready, [None, Some(Duration::from_millis(50)), None, None])?;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the deadline must end the wait"
    );
    Ok(())
}

#[test]
fn selecting_a_busy_child_keeps_its_observed_activity() -> TestResult {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            frame: None,
            hold: true,
            fail_history: false,
            keep_sessions: true,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().ok_or("project")?.clone();
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client.models().first().ok_or("model")?.clone();
    client.select_model(&model)?;
    let child = client.create_session()?;
    let other = client.create_session()?;
    assert_ne!(child, other);
    client.peer_mut().frame = Some(FramePoll::Frame(
        serde_json::json!({
            "id":"evt_busy_child", "type":"session.status", "created":1_791_695_003_000_u64,
            "data":{"sessionID":child.spelled(), "status":{"type":"busy"},
                "state":{"type":"running","revision":1_000,"executionID":"exe_busy_child"}}
        })
        .to_string(),
    ));
    // The connect, then the busy edge.
    client.pump()?;
    client.pump()?;
    assert_eq!(
        client.observed_activity(&child),
        Some(grapt_client::client::SessionActivity::Running)
    );
    client.select(&child)?;
    assert_eq!(selected_activity(&client), Some(true));
    assert!(
        selected_running(&client),
        "selecting a busy session must retain its activity"
    );
    Ok(())
}
