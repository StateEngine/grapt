use super::*;
use crate::keys::{handle_key, paste};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::{
    wire::Spelled, Bounds, ContractDouble, FramePoll, Operation, Transport, TransportError,
    WireRequest, WireResponse,
};
use std::sync::mpsc;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn fixture() -> (Ui, Backend, crate::Commands) {
    let (commands, rx) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let mut ui = Ui::new(false);
    ui.launch.start(Some("/literal\r\nsecond\tline\n".into()));
    (
        ui,
        Backend {
            commands,
            updates,
            wake,
        },
        rx,
    )
}

#[test]
fn hidden_prepared_input_waits_for_choices_and_sends_once() -> Result {
    let (mut ui, backend, commands) = fixture();
    assert!(draft(&ui).is_empty());
    assert!(!finish(&mut ui, &backend));
    for state in [Readiness::Loading, Readiness::Unresolved] {
        ui.launch.update(state);
        ui.launch.painted();
        assert!(!finish(&mut ui, &backend));
        assert!(draft(&ui).is_empty());
        assert!(commands.try_recv().is_err());
    }
    ui.launch.painted = false;
    ui.launch.update(Readiness::Ready);
    assert!(!finish(&mut ui, &backend), "must paint before sending");
    ui.launch.painted();
    assert!(finish(&mut ui, &backend));
    let Command::StartupPrompt(text) = commands.try_recv()? else {
        return Err("missing startup send".into());
    };
    assert_eq!(text, "/literal\r\nsecond\tline\n");
    assert!(draft(&ui).is_empty());
    assert_eq!(ui.pending.as_ref(), Some(&text));
    // A failed/uncertain prepared send never becomes a human draft or rearms intent.
    ui.pending = None;
    ui.launch.update(Readiness::Ready);
    for _ in 0..5 {
        assert!(!finish(&mut ui, &backend));
    }
    assert!(commands.try_recv().is_err());
    assert!(draft(&ui).is_empty());
    Ok(())
}

#[test]
fn editing_navigation_and_explicit_enter_cancel_delayed_submission() -> Result {
    for key in [
        KeyCode::Char('x'),
        KeyCode::Left,
        KeyCode::Esc,
        KeyCode::Enter,
        KeyCode::PageUp,
        KeyCode::F(3),
    ] {
        let (mut ui, backend, commands) = fixture();
        ui.launch.painted();
        handle_key(KeyEvent::new(key, KeyModifiers::NONE), &mut ui, &backend)?;
        ui.launch.update(Readiness::Ready);
        assert!(!finish(&mut ui, &backend), "{key:?}");
        while let Ok(command) = commands.try_recv() {
            assert!(!matches!(
                command,
                Command::Submit(_) | Command::StartupPrompt(_) | Command::Retry(_)
            ));
        }
        assert_eq!(draft(&ui), if key == KeyCode::Char('x') { "x" } else { "" });
    }
    let (mut ui, backend, commands) = fixture();
    assert!(paste("edited", &mut ui));
    ui.launch.update(Readiness::Ready);
    ui.launch.painted();
    assert!(!finish(&mut ui, &backend));
    assert!(commands.try_recv().is_err());
    Ok(())
}

#[test]
fn failed_reads_wait_for_explicit_retry_and_protected_choices_become_manual() -> Result {
    let (mut ui, backend, commands) = fixture();
    ui.launch.update(Readiness::Paused);
    ui.launch.painted();
    for _ in 0..5 {
        assert!(!finish(&mut ui, &backend));
    }
    assert!(commands.try_recv().is_err());
    handle_key(
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        &mut ui,
        &backend,
    )?;
    assert!(matches!(commands.try_recv()?, Command::StartupRead));
    assert_eq!(ui.launch.readiness, Readiness::Connecting);
    ui.launch.update(Readiness::Ready);
    assert!(finish(&mut ui, &backend));
    let (mut ui, backend, commands) = fixture();
    ui.launch.update(Readiness::Manual);
    ui.launch.update(Readiness::Ready);
    ui.launch.painted();
    assert!(!finish(&mut ui, &backend));
    assert!(commands.try_recv().is_err());
    assert!(draft(&ui).is_empty());
    Ok(())
}

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    saved: Vec<u8>,
    calls: Vec<Operation>,
    fail: bool,
    disabled: bool,
    /// Answer snapshots with a transient 503.
    defer_snapshot: bool,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> std::result::Result<WireResponse, TransportError> {
        assert!(self.calls.len() < 256, "fixture call bound");
        self.calls.push(request.operation());
        if request.operation() == Operation::SessionGet {
            return Ok(WireResponse {
                status: if self.fail { 503 } else { 200 },
                body: self.saved.clone(),
            });
        }
        if self.defer_snapshot && request.operation() == Operation::SessionSnapshot {
            return Ok(WireResponse {
                status: 503,
                body: Vec::new(),
            });
        }
        let mut response = self.inner.call(request)?;
        if request.operation() == Operation::ModelList {
            let mut body: serde_json::Value =
                serde_json::from_slice(&response.body).map_err(|e| TransportError::Io {
                    reason: e.to_string(),
                })?;
            body["data"][0]["variants"] = serde_json::json!([{"id":"high"}]);
            body["data"][0]["enabled"] = serde_json::json!(!self.disabled);
            response.body = serde_json::to_vec(&body).map_err(|e| TransportError::Io {
                reason: e.to_string(),
            })?;
        }
        Ok(response)
    }
}
impl Connection for Peer {
    fn poll_frame(&mut self) -> std::result::Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
}

fn client() -> Result<(Client<Peer>, SessionWireId)> {
    let mut inner = ContractDouble::new();
    let session = inner
        .create_session("Prepared R seat")
        .ok_or("session bound")?;
    let mut client = Client::new(
        Peer {
            inner,
            saved: vec![],
            calls: vec![],
            fail: false,
            disabled: false,
            defer_snapshot: false,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().ok_or("project offer")?.clone();
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent offer")?.id.clone();
    client.select_agent(&agent)?;
    let model = client.models().first().ok_or("model offer")?.clone();
    client.select_model(&model)?;
    let project = client.project().ok_or("project")?;
    let mut info = serde_json::to_value(grapt_client::wire::session_info(
        &session,
        "Prepared R seat",
        &project.worktree,
        None,
    )?)?;
    info["projectID"] = serde_json::json!(project.id.spelled());
    info["agent"] = serde_json::json!(client.agent().ok_or("agent")?.spelled());
    let model = client.model().ok_or("model")?;
    info["model"] = serde_json::json!({"id":model.id.spelled(),"providerID":model.provider_id.spelled(),"variant":"high"});
    client.peer_mut().saved = serde_json::to_vec(&serde_json::json!({"data":info}))?;
    client.peer_mut().calls.clear();
    Ok((client, session))
}

/// Attach, then pump until the attached Session's snapshot settles readiness.
fn attached(
    client: &mut Client<Peer>,
    session: &SessionWireId,
) -> std::result::Result<Readiness, Box<dyn std::error::Error>> {
    let mut readiness = attach(client, session)?;
    for _ in 0..8 {
        if readiness != Readiness::Loading {
            break;
        }
        client.pump()?;
        readiness = crate::launch::readiness(client);
    }
    Ok(readiness)
}

#[test]
fn attachment_restores_exact_effort_without_selection_or_creation_writes() -> Result {
    let (mut client, session) = client()?;
    client.peer_mut().fail = true;
    assert!(attach(&mut client, &session).is_err());
    client.peer_mut().fail = false;
    assert_eq!(attached(&mut client, &session)?, Readiness::Ready);
    assert_eq!(
        client
            .peer_mut()
            .calls
            .iter()
            .filter(|op| **op == Operation::PermissionRequestList)
            .count(),
        1,
        "the canonical attachment read also supplies startup protection"
    );
    assert_eq!(client.selected(), Some(&session));
    assert_eq!(
        client
            .model()
            .and_then(|model| model.variant.as_ref())
            .map(Spelled::spelled),
        Some("high")
    );
    assert!(!client.peer_mut().calls.iter().any(|op| matches!(
        op,
        Operation::SessionCreate
            | Operation::SessionPrompt
            | Operation::SessionAgentSelect
            | Operation::SessionModelSelect
            | Operation::SessionPresetSelect
    )));
    Ok(())
}

#[test]
fn readiness_waits_for_the_selected_sessions_own_snapshot() -> Result {
    let (mut client, session) = client()?;
    assert_eq!(attached(&mut client, &session)?, Readiness::Ready);
    let other = client
        .peer_mut()
        .inner
        .create_session("second")
        .ok_or("session bound")?;
    client.refresh_sessions()?;
    // The stream stays live while the newly selected Session's own snapshot
    // is deferred.
    client.peer_mut().defer_snapshot = true;
    client.select(&other)?;
    assert!(client.stream_state().is_live());
    assert_eq!(readiness(&client), Readiness::Loading);
    client.peer_mut().defer_snapshot = false;
    for _ in 0..4 {
        client.pump()?;
    }
    assert_eq!(readiness(&client), Readiness::Ready);
    Ok(())
}

#[test]
fn missing_choices_and_forms_never_authorize_automatic_submission() -> Result {
    for case in ["missing", "disabled", "form", "approval"] {
        let (mut client, session) = client()?;
        match case {
            "missing" => {
                let mut saved: serde_json::Value =
                    serde_json::from_slice(&client.peer_mut().saved)?;
                saved["data"]
                    .as_object_mut()
                    .ok_or("saved session object")?
                    .remove("agent");
                client.peer_mut().saved = serde_json::to_vec(&saved)?;
            }
            "disabled" => client.peer_mut().disabled = true,
            "approval" => {
                client
                    .peer_mut()
                    .inner
                    .ask_permission(&session, "shell: test")
                    .ok_or("permission bound")?;
            }
            _ => {
                client
                    .peer_mut()
                    .inner
                    .raise_form(&session, "Choose a branch")
                    .ok_or("form bound")?;
            }
        }
        let readiness =
            attached(&mut client, &session).map_err(|error| format!("{case}: {error}"))?;
        assert_eq!(readiness, Readiness::Manual, "{case}");
        assert!(!client.peer_mut().calls.contains(&Operation::SessionPrompt));
    }
    Ok(())
}

#[test]
fn submission_rechecks_new_and_externally_settled_requests_without_replaying() -> Result {
    let (mut client, session) = client()?;
    assert_eq!(attached(&mut client, &session)?, Readiness::Ready);
    client
        .peer_mut()
        .inner
        .ask_permission(&session, "shell: newly pending")
        .ok_or("permission bound")?;
    assert!(submit(&mut client, "manual draft").is_err());
    assert!(!client.peer_mut().calls.contains(&Operation::SessionPrompt));
    // The server has settled its request while this client still holds the old ask.
    client.peer_mut().inner.withdraw_permissions(&session);
    assert!(protected(&client));
    submit(&mut client, "manual draft")?;
    assert_eq!(
        client
            .peer_mut()
            .calls
            .iter()
            .filter(|op| **op == Operation::SessionPrompt)
            .count(),
        1
    );
    assert_eq!(client.peer_mut().inner.user_inputs(&session).len(), 1);
    Ok(())
}

#[test]
fn read_only_command_result_cancels_delayed_startup_without_touching_its_draft() -> Result {
    let (mut ui, backend, commands) = fixture();
    let before = draft(&ui);
    ui.launch.update(Readiness::Ready);
    ui.launch.painted();
    ui.command_pane = Some(crate::command_pane::Pane::new(vec![
        "read-only result".into()
    ]));
    assert!(!finish(&mut ui, &backend));
    ui.command_pane = None;
    assert!(!finish(&mut ui, &backend));
    assert_eq!(draft(&ui), before);
    assert!(commands.try_recv().is_err());
    Ok(())
}

#[test]
fn automatic_startup_retains_a_recognized_command_as_literal_prompt_text() -> Result {
    let (mut ui, backend, commands) = fixture();
    ui.launch.start(Some("/compact".into()));
    ui.launch.update(Readiness::Ready);
    ui.launch.painted();
    assert!(finish(&mut ui, &backend));
    assert!(matches!(commands.try_recv()?, Command::StartupPrompt(text) if text == "/compact"));
    assert!(!finish(&mut ui, &backend));
    Ok(())
}

#[test]
fn loading_catalog_keeps_input_and_never_replays_early_enter() -> Result {
    for state in [Readiness::Loading, Readiness::Unresolved] {
        let (mut ui, backend, commands) = fixture();
        assert!(paste("human draft", &mut ui));
        ui.launch.update(state);
        ui.launch.painted();
        assert!(!finish(&mut ui, &backend));
        let original = draft(&ui);
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert!(commands.try_recv().is_err());
        assert_eq!(draft(&ui), original);
        ui.launch.update(Readiness::Ready);
        assert!(!finish(&mut ui, &backend));
        assert!(commands.try_recv().is_err());
        handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert!(matches!(commands.try_recv()?, Command::Submit(_)));
    }
    Ok(())
}

#[test]
fn saved_choices_and_launch_effort_validate_before_submission() -> Result {
    let path = std::env::temp_dir().join(format!("r-metadata-catalog-{}", std::process::id()));
    std::fs::create_dir(&path)?;
    let checked = (|| -> Result {
        for (disabled, level) in [(false, ""), (true, ""), (false, "high"), (false, "unknown")] {
            let (mut client, session) = client()?;
            if !level.is_empty() {
                let args = ["--service", "/tmp/fixture", "--session", session.spelled()];
                let config = crate::config::Config::parse_args(
                    args.into_iter()
                        .chain(["--reasoning-effort", level])
                        .map(str::to_owned),
                )?;
                client.set_launch_effort(config.reasoning_effort);
            }
            client.set_catalog_profile(&path);
            client.peer_mut().disabled = disabled;
            assert_eq!(attach(&mut client, &session)?, Readiness::Loading);
            if !level.is_empty() {
                assert!(submit_startup(&mut client, "early").is_err());
            }
            for _ in 0..3 {
                client.pump()?;
            }
            let manual = disabled || level == "unknown";
            let expected = if manual {
                Readiness::Manual
            } else {
                Readiness::Ready
            };
            assert_eq!(readiness(&client), expected);
            if level == "unknown" {
                assert!(client.catalog_status().ok_or("refusal")?.contains(level));
                assert!(submit_startup(&mut client, "refused").is_err());
            }
            let calls = &client.peer_mut().calls;
            assert!(!calls
                .iter()
                .any(|op| matches!(op, Operation::SessionPrompt | Operation::SessionCreate)));
            if level != "high" {
                assert!(!calls.contains(&Operation::SessionModelSelect));
                continue;
            }
            let request = WireRequest::new(Operation::SessionGet, Some(&session), None)?;
            let bytes = client.peer_mut().inner.call(&request)?.body;
            let saved = grapt_client::wire::decode_session_model(&bytes)?.ok_or("saved model")?;
            assert_eq!(saved.variant.as_ref().map(Spelled::spelled), Some("high"));
            submit_startup(&mut client, "start")?;
            // F3 uses this same owner to change the saved effort afterwards.
            let base = client
                .models()
                .iter()
                .find(|m| m.variant.is_none())
                .ok_or("base model")?
                .clone();
            client.select_model(&base)?;
            let bytes = client.peer_mut().inner.call(&request)?.body;
            assert!(grapt_client::wire::decode_session_model(&bytes)?
                .ok_or("saved model")?
                .variant
                .is_none());
        }
        Ok(())
    })();
    std::fs::remove_dir_all(path)?;
    checked
}
