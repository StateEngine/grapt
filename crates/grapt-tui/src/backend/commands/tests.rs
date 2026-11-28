use super::*;
use grapt_client::{
    transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse},
    wire::{decode_prompt, Spelled},
    Bounds, ContractDouble, Fault, Operation,
};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct Peer {
    inner: ContractDouble,
    calls: Vec<Operation>,
    prompts: Vec<String>,
    refuse_prompt: bool,
    hide_sessions: bool,
    reconciled: u64,
    refuse_reconcile: bool,
    large_template: bool,
    auth: Vec<serde_json::Value>,
    auth_sessions: Vec<Option<String>>,
    mcp_queries: Vec<Vec<(String, String)>>,
    nested_session: bool,
    auth_socket: Option<grapt_client::HttpConnection>,
    next_auth_status: Option<u16>,
    export_pages: Option<std::collections::VecDeque<WireResponse>>,
    instruction_response: Option<WireResponse>,
    /// The stream subscription a too-old service refuses, naming what it lacks.
    refuse_subscribe: Option<&'static str>,
}
impl Transport for Peer {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        assert!(self.calls.len() < 256, "command fixture request bound");
        self.calls.push(request.operation());
        if mcp_auth::handles(request.operation()) {
            return mcp_auth::response(self, request);
        }
        if matches!(
            request.operation(),
            Operation::McpInspect | Operation::McpList
        ) {
            self.mcp_queries.push(request.query().to_vec());
        }
        if request.operation() == Operation::MessageList {
            if let Some(pages) = &mut self.export_pages {
                assert!(request
                    .query()
                    .iter()
                    .any(|(k, v)| k == "limit" && v == "200"));
                assert!(
                    !request.query().iter().any(|(k, _)| k == "cursor")
                        || !request.query().iter().any(|(k, _)| k == "order")
                );
                assert!(request
                    .query()
                    .iter()
                    .all(|(k, v)| k != "order" || v == "asc"));
                return pages.pop_front().ok_or(TransportError::NotConnected);
            }
        }
        if request.operation() == Operation::InstructionInspect {
            if let Some(response) = &self.instruction_response {
                return Ok(response.clone());
            }
        }
        if request.operation() == Operation::SessionReconcile && self.refuse_reconcile {
            return Ok(WireResponse {
                status: 503,
                body: Vec::new(),
            });
        }
        if request.operation() == Operation::SessionPrompt {
            let body = request.body().ok_or(TransportError::NotConnected)?;
            self.prompts
                .push(decode_prompt(body).map_err(|_| TransportError::NotConnected)?);
            if self.refuse_prompt {
                return Ok(WireResponse {
                    status: 503,
                    body: Vec::new(),
                });
            }
        }
        let mut response =
            if self.nested_session && request.operation() == Operation::ProjectCurrent {
                self.inner.call(
                    &WireRequest::new(Operation::ProjectCurrent, None, None)
                        .map_err(|_| TransportError::NotConnected)?
                        .with_query("location[directory]", "/work/project-a"),
                )?
            } else {
                self.inner.call(request)?
            };
        if self.nested_session && request.operation() == Operation::SessionGet {
            let mut body: serde_json::Value =
                serde_json::from_slice(&response.body).map_err(|_| TransportError::NotConnected)?;
            body["data"]["location"]["directory"] = "/work/project-a/nested".into();
            response.body = serde_json::to_vec(&body).map_err(|_| TransportError::NotConnected)?;
        }
        if request.operation() == Operation::SessionReconcile && response.status == 200 {
            let mut body: serde_json::Value =
                serde_json::from_slice(&response.body).map_err(|_| TransportError::NotConnected)?;
            body["data"]["reconciled"] = self.reconciled.into();
            response.body = serde_json::to_vec(&body).map_err(|_| TransportError::NotConnected)?;
        }
        if request.operation() == Operation::CommandInspect && response.status == 200 {
            let mut body: serde_json::Value =
                serde_json::from_slice(&response.body).map_err(|_| TransportError::NotConnected)?;
            body["data"] = serde_json::json!([
                {"name":"review","description":"Review changes","template":"Review $ARGUMENTS"},
                {"name":"focus","template":"Read $1 then $2"},
                {"name":"special","template":"Task","subtask":true},
                {"name":"slash-task","template":"  /work $ARGUMENTS"}
            ]);
            for row in body["data"]
                .as_array_mut()
                .ok_or(TransportError::NotConnected)?
            {
                let builtin = row["name"] == "review";
                *row = serde_json::json!({"command":row.clone(),"origin":if builtin {"builtin"} else {"user"},"source":if builtin {"Grapt built-in"} else {"/home/example/config/grapt.json"}});
            }
            body["data"]
                .as_array_mut()
                .ok_or(TransportError::NotConnected)?
                .push(serde_json::json!({
                    "command":{"name":"blocked-project","template":"MUST_NOT_SEND"},
                    "origin":"project","source":"/work/project-a/grapt.jsonc",
                    "unavailable":"Repository is not trusted"
                }));
            if self.large_template {
                body["data"][1]["command"]["template"] = "x".repeat(60_000).into();
            }
            response.body = serde_json::to_vec(&body).map_err(|_| TransportError::NotConnected)?;
        }
        if request.operation() == Operation::ModelList && response.status == 200 {
            let mut body: serde_json::Value =
                serde_json::from_slice(&response.body).map_err(|_| TransportError::NotConnected)?;
            body["data"][0]["variants"] = serde_json::json!([{"id":"high"},{"id":"low"}]);
            response.body = serde_json::to_vec(&body).map_err(|_| TransportError::NotConnected)?;
        }
        if request.operation() == Operation::SessionList && self.hide_sessions {
            response.body = grapt_client::wire::encode_sessions(Vec::new())
                .map_err(|_| TransportError::NotConnected)?;
        }
        Ok(response)
    }
}
impl Connection for Peer {
    fn begin_operator(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        match &mut self.auth_socket {
            Some(socket) => socket.begin_operator(request),
            None => self.call(request).map(Some),
        }
    }
    fn poll_operator(&mut self) -> Result<Option<WireResponse>, TransportError> {
        self.auth_socket
            .as_mut()
            .ok_or(TransportError::NotConnected)?
            .poll_operator()
    }
    fn cancel_operator(&mut self) {
        if let Some(socket) = &mut self.auth_socket {
            socket.cancel_operator();
        }
    }

    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe()
    }
    fn subscribed(&self) -> bool {
        self.refuse_subscribe.is_none() && self.inner.subscribed()
    }
    fn begin_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        match self.refuse_subscribe {
            Some(missing) => Err(TransportError::ServerTooOld { missing }),
            None => {
                self.inner.resubscribe();
                Ok(Some(self.inner.subscribed()))
            }
        }
    }
}
fn prepared(session: bool) -> TestResult<Client<Peer>> {
    prepared_with_bounds(session, Bounds::PRODUCTION)
}
fn prepared_with_bounds(session: bool, bounds: Bounds) -> TestResult<Client<Peer>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            calls: Vec::new(),
            prompts: Vec::new(),
            refuse_prompt: false,
            hide_sessions: false,
            reconciled: 0,
            refuse_reconcile: false,
            large_template: false,
            auth: Vec::new(),
            auth_sessions: Vec::new(),
            mcp_queries: Vec::new(),
            nested_session: false,
            auth_socket: None,
            next_auth_status: None,
            export_pages: None,
            instruction_response: None,
            refuse_subscribe: None,
        },
        bounds,
    );
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project")?;
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client
        .models()
        .iter()
        .find(|m| m.enabled && m.variant.as_ref().is_some_and(|v| v.spelled() == "high"))
        .cloned()
        .ok_or("model")?;
    client.select_model(&model)?;
    if session {
        client.create_session()?;
    }
    Ok(client)
}
fn run(client: &mut Client<Peer>, text: &str) -> TestResult<Completion> {
    execute(client, text).ok_or_else(|| "command not recognized".into())
}

mod export;
mod fork;
mod inspection;
mod reconciliation;
mod refusal;

#[test]
fn commands_use_client_routes_instead_of_becoming_model_prompts() -> TestResult {
    let mut client = prepared(true)?;
    assert!(matches!(
        run(&mut client, "/status")?.view,
        Some(View::Status)
    ));
    assert!(client.peer_mut().calls.contains(&Operation::ProviderList));
    assert!(matches!(
        run(&mut client, "/model")?.view,
        Some(View::Models)
    ));
    for command in ["/compact", "/compact\textra"] {
        let compact = run(&mut client, command)?;
        assert!(compact.consume && compact.notice.contains("next message"));
    }
    assert!(client.peer_mut().calls.contains(&Operation::SessionCompact));
    for (command, title, route) in [
        ("/diff", "WORKSPACE DIFF", Operation::WorkspaceDiff),
        ("/mcp", "MCP SERVERS", Operation::McpInspect),
        ("/mcp VeRbOsE", "MCP DETAILS", Operation::McpInspect),
        ("/mcps", "MCP SERVERS", Operation::McpList),
    ] {
        let completion = run(&mut client, command)?;
        assert!(completion.consume);
        let Some(View::Text(rows) | View::Mcp { rows, .. }) = completion.view else {
            return Err("missing read-only result".into());
        };
        assert!(rows.join("\n").contains(title));
        assert!(client.peer_mut().calls.contains(&route));
    }
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn command_aliases_keep_the_original_draft_identity_and_existing_dispatch() -> TestResult {
    let mut client = prepared(true)?;
    let text = "/models";
    let done = run(&mut client, text)?;
    assert_eq!(done.text, text);
    assert!(matches!(done.view, Some(View::Models)));
    assert!(!submits_prompt(text));
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn bad_arguments_preserve_command_and_never_call_a_provider() -> TestResult {
    let mut client = prepared(true)?;
    let session = client.selected().cloned();
    let count = client.peer_mut().inner.session_count();
    for text in [
        "/model nope",
        "/new extra",
        "/diff extra",
        "/init replace",
        "/mcp login",
        "/mcp invalid",
    ] {
        let done = run(&mut client, text)?;
        assert!(!done.consume && done.replacement.is_none());
        assert_eq!(done.text, text);
        assert!(done.notice.contains("Use /") || done.notice.contains("sign-in"));
    }
    assert_eq!(client.selected(), session.as_ref());
    assert_eq!(client.peer_mut().inner.session_count(), count);
    assert!(client.peer_mut().prompts.is_empty());
    for text in [
        "/modelish",
        "/newer",
        "/resumeable",
        "/initial",
        "/mcpish",
        "/status extra",
        "/diff\textra",
    ] {
        let done = run(&mut client, text)?;
        assert!(!done.consume, "invalid or unknown command consumed: {text}");
    }
    Ok(())
}

#[test]
fn new_resume_titles_ids_ambiguity_and_saved_choices_use_existing_attachment() -> TestResult {
    let mut client = prepared(true)?;
    let first = client.selected().cloned().ok_or("first")?;
    let choice = client.model().cloned();
    assert_eq!(
        choice
            .as_ref()
            .and_then(|m| m.variant.as_ref())
            .map(Spelled::spelled),
        Some("high")
    );
    client.rename("First saved session")?;
    assert!(run(&mut client, "/new")?.consume);
    let second = client.selected().cloned().ok_or("second")?;
    assert_ne!(first, second);
    assert_eq!(client.model().cloned(), choice);
    client.rename("Second saved session")?;
    assert!(run(&mut client, "/resume First saved session")?.consume);
    assert_eq!(client.selected(), Some(&first));
    assert_eq!(client.model().cloned(), choice);
    client.peer_mut().hide_sessions = true;
    client.refresh_sessions()?;
    assert!(client.sessions().is_empty());
    assert!(run(&mut client, &format!("/resume {}", second.spelled()))?.consume);
    client.peer_mut().hide_sessions = false;
    assert_eq!(client.selected(), Some(&second));
    assert!(!run(&mut client, "/resume missing title")?.consume);
    assert_eq!(client.selected(), Some(&second));
    assert!(!run(&mut client, "/resume ses_999999999999")?.consume);
    assert_eq!(client.selected(), Some(&second));
    client.rename("First saved session")?;
    assert!(run(&mut client, "/resume First saved session")?
        .notice
        .contains("More than one"));
    assert_eq!(client.selected(), Some(&second));
    assert!(matches!(
        run(&mut client, "/resume")?.view,
        Some(View::Sessions)
    ));
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn init_shares_the_guidance_task_and_refusal_keeps_it_editable() -> TestResult {
    let mut no_session = prepared(false)?;
    assert!(!run(&mut no_session, "/init")?.consume);
    assert!(no_session.peer_mut().prompts.is_empty());
    let mut client = prepared(true)?;
    client.peer_mut().refuse_prompt = true;
    let done = run(&mut client, "/init")?;
    assert!(!done.consume);
    assert_eq!(done.replacement.as_deref(), Some(INIT_PROMPT.trim_end()));
    assert_eq!(client.peer_mut().prompts, vec![INIT_PROMPT.trim_end()]);
    client.peer_mut().refuse_prompt = false;
    assert!(run(&mut client, "/init")?.consume);
    assert!(INIT_PROMPT.contains("do not overwrite or modify it"));
    Ok(())
}

#[test]
fn local_inspection_preserves_uncertain_submission_and_init_never_replaces_its_identity(
) -> TestResult {
    let mut client = prepared(true)?;
    let session = client.selected().cloned().ok_or("session")?;
    client
        .peer_mut()
        .inner
        .set_fault(Fault::PromptReplyLostUnconfirmed);
    assert!(client.submit_prompt("ORIGINAL uncertain prompt").is_err());
    assert!(run(&mut client, "/status")?.consume);
    assert!(run(&mut client, "/diff")?.consume);
    let done = run(&mut client, "/init")?;
    assert!(!done.consume);
    assert_eq!(client.peer_mut().prompts, vec!["ORIGINAL uncertain prompt"]);
    // Confirmation resolves the held original; the init command remains an
    // editable ordinary task and was never sent under the original input id.
    assert_eq!(client.peer_mut().inner.user_inputs(&session).len(), 1);
    assert!(client.submit_prompt(INIT_PROMPT.trim_end()).is_ok());
    assert_eq!(client.peer_mut().prompts.len(), 2);
    Ok(())
}

#[test]
fn mcp_refusal_is_visible_and_help_names_supported_commands() -> TestResult {
    let mut client = prepared(true)?;
    client.peer_mut().inner.set_fault(Fault::McpUnavailable);
    let done = run(&mut client, "/mcp verbose")?;
    assert!(done.consume);
    let Some(View::Text(rows) | View::Mcp { rows, .. }) = done.view else {
        return Err("refusal pane missing".into());
    };
    assert!(rows.join("\n").contains("400"));
    let Some(View::Text(rows)) = run(&mut client, "/help")?.view else {
        return Err("help missing".into());
    };
    let help = rows.join("\n");
    for command in [
        "/status",
        "/model",
        "/compact",
        "/reconcile",
        "/new",
        "/resume",
        "/diff",
        "/mcp",
        "/init",
        "Alt+R",
    ] {
        assert!(help.contains(command), "{command}");
    }
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn command_completion_stays_with_its_session_across_queued_selections() -> TestResult {
    let mut client = prepared(true)?;
    let mut status = run(&mut client, "/status")?;
    assert!(status.keep_for(client.selected()));
    assert!(status.consume && matches!(status.view, Some(View::Status)));
    let mut model = run(&mut client, "/model")?;
    client.peer_mut().refuse_prompt = true;
    let mut init = run(&mut client, "/init")?;
    assert!(init.replacement.is_some());
    client.create_session()?;
    for done in [&mut status, &mut model, &mut init] {
        assert!(!done.keep_for(client.selected()));
        assert!(!done.consume && done.view.is_none() && done.replacement.is_none());
        assert!(done.notice.contains("Session changed"));
    }
    let mut new = run(&mut client, "/new")?;
    assert!(
        new.keep_for(client.selected()),
        "new completion targets its created session"
    );
    assert!(new.consume);
    Ok(())
}

#[test]
fn init_uses_the_same_pending_request_guard_as_ordinary_submission() -> TestResult {
    for kind in ["form", "approval"] {
        let mut client = prepared(true)?;
        let selected = client.selected().cloned().ok_or("session")?;
        if kind == "form" {
            client
                .peer_mut()
                .inner
                .raise_form(&selected, "Choose a branch")
                .ok_or("form bound")?;
        } else {
            client
                .peer_mut()
                .inner
                .ask_permission(&selected, "shell: test")
                .ok_or("permission bound")?;
        }
        let done = run(&mut client, "/init")?;
        assert!(!done.consume);
        assert_eq!(done.replacement.as_deref(), Some(INIT_PROMPT.trim_end()));
        assert!(
            client.peer_mut().prompts.is_empty(),
            "{kind}: init bypassed the ordinary guard"
        );
    }
    Ok(())
}

#[test]
fn workspace_commands_expand_then_use_the_existing_submission_owner() -> TestResult {
    let mut client = prepared(true)?;
    client.enable_commands();
    client.pump()?;
    let review = run(&mut client, "/review feature")?;
    assert!(review.consume);
    assert_eq!(
        client.peer_mut().prompts.last().map(String::as_str),
        Some("Review feature")
    );
    client.pump()?;
    let focus = run(&mut client, "/focus \"a b.rs\" '界.rs'")?;
    assert!(!focus.consume);
    assert_eq!(focus.replacement.as_deref(), Some("Read a b.rs then 界.rs"));
    assert!(client
        .peer_mut()
        .prompts
        .iter()
        .all(|text| text != "Read a b.rs then 界.rs"));
    client.submit_prompt(focus.replacement.as_deref().ok_or("preview")?)?;
    assert_eq!(
        client.peer_mut().prompts.last().map(String::as_str),
        Some("Read a b.rs then 界.rs")
    );
    let count = client.peer_mut().prompts.len();
    for text in ["/focus one", "/special"] {
        let kept = run(&mut client, text)?;
        assert!(!kept.consume);
        assert!(kept.replacement.is_none());
    }
    assert_eq!(client.peer_mut().prompts.len(), count);
    Ok(())
}

#[test]
fn refused_custom_command_keeps_expansion_for_the_same_prompt_retry() -> TestResult {
    let mut client = prepared(true)?;
    client.enable_commands();
    client.pump()?;
    client.peer_mut().refuse_prompt = true;
    let result = run(&mut client, "/review feature")?;
    assert!(!result.consume);
    assert_eq!(result.text, "/review feature");
    assert_eq!(result.replacement.as_deref(), Some("Review feature"));
    Ok(())
}

#[test]
fn rename_and_rewind_use_saved_session_routes_and_reopen_without_sending() -> TestResult {
    let mut client = prepared(true)?;
    let session = client.selected().cloned();
    let choice = client.model().cloned();
    assert!(run(&mut client, "/rename Named from R")?.consume);
    assert!(client
        .sessions()
        .iter()
        .any(|row| row.title == "Named from R"));
    assert!(client.peer_mut().calls.contains(&Operation::SessionRename));
    client.submit_prompt("original prompt to reopen")?;
    let sent = client.peer_mut().prompts.len();
    let done = run(&mut client, "/rewind")?;
    assert_eq!(
        done.replacement.as_deref(),
        Some("original prompt to reopen")
    );
    assert!(done.notice.contains("files unchanged"));
    let mut ui = crate::ui::Ui::new(false);
    ui.composer.insert_str("/rewind");
    ui.command_update(done);
    assert_eq!(crate::keys::draft(&ui), "original prompt to reopen");
    assert!(
        ui.command_pane.is_none(),
        "confirmation must not block editing"
    );
    // Stream resubscription changes service status after a successful rewind.
    for status in ["Service reconnecting; draft kept", "Connected"] {
        ui.status = status.into();
        assert!(ui
            .command_notice
            .as_deref()
            .is_some_and(|text| text.contains("files unchanged")));
    }
    assert!(crate::recovery::rows(&ui, 160)
        .join("\n")
        .contains("files unchanged"));
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = std::sync::mpsc::sync_channel(1);
    let backend = crate::backend::Backend {
        commands,
        updates,
        wake,
    };
    crate::keys::handle_key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('!'),
            crossterm::event::KeyModifiers::NONE,
        ),
        &mut ui,
        &backend,
    )?;
    assert!(ui.command_notice.is_none());
    assert!(crate::keys::draft(&ui).contains('!'));
    assert!(
        received.try_recv().is_err(),
        "editing sent a backend command"
    );
    assert!(client
        .peer_mut()
        .calls
        .contains(&Operation::SessionRewindPreview));
    assert!(client.peer_mut().calls.contains(&Operation::SessionRewind));
    assert_eq!(client.peer_mut().prompts.len(), sent);
    assert_eq!(client.selected(), session.as_ref());
    assert_eq!(client.model(), choice.as_ref());
    Ok(())
}

#[test]
fn undo_requires_review_and_refused_or_invalid_commands_keep_the_draft() -> TestResult {
    let mut client = prepared(true)?;
    let done = run(&mut client, "/undo apply")?;
    assert!(!done.consume && done.notice.contains("review"));
    assert!(!client.peer_mut().calls.contains(&Operation::SessionUndo));
    let done = run(&mut client, "/undo")?;
    assert!(!done.consume, "the double has no patch filesystem");
    assert!(client
        .peer_mut()
        .calls
        .contains(&Operation::SessionUndoPreview));
    for text in [
        "/rename",
        "/rewind 0",
        "/rewind 201",
        "/rewind no",
        "/undo discard",
    ] {
        let before = client.peer_mut().calls.len();
        let done = run(&mut client, text)?;
        assert!(!done.consume && done.replacement.is_none());
        assert_eq!(done.text, text);
        assert_eq!(client.peer_mut().calls.len(), before);
        assert!(!submits_prompt(text));
    }
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn unknown_commands_are_local_and_double_slash_is_an_explicit_literal() -> TestResult {
    let mut client = prepared(true)?;
    client.enable_commands();
    client.pump()?;
    for text in [
        "/unknown",
        "/unknown arguments",
        "/renameish",
        "/quit extra",
    ] {
        let done = run(&mut client, text)?;
        assert!(!done.consume);
        assert_eq!(done.text, text);
        assert!(done.notice.contains("Unknown command") || done.notice.contains("Use /quit"));
    }
    assert!(client.peer_mut().prompts.is_empty());
    assert!(run(&mut client, "//unknown arguments")?.consume);
    client.pump()?;
    assert!(run(&mut client, "//review feature")?.consume);
    assert_eq!(
        client.peer_mut().prompts,
        ["/unknown arguments", "/review feature"]
    );
    Ok(())
}

#[test]
fn literal_refusal_preserves_the_escape_and_never_redispatches() -> TestResult {
    let mut client = prepared(true)?;
    client.peer_mut().refuse_prompt = true;
    let done = run(&mut client, "//rename not a command")?;
    assert!(!done.consume);
    assert!(done.replacement.is_none());
    assert_eq!(done.text, "//rename not a command");
    client.peer_mut().refuse_prompt = false;
    assert!(run(&mut client, &done.text)?.consume);
    assert_eq!(
        client.peer_mut().prompts,
        ["/rename not a command", "/rename not a command"]
    );
    assert!(!client.peer_mut().calls.contains(&Operation::SessionRename));
    Ok(())
}

#[test]
fn unknown_command_distinguishes_an_unavailable_catalog_from_absence() -> TestResult {
    let mut client = prepared(true)?;
    client.enable_commands();
    let done = run(&mut client, "/missing")?;
    assert!(!done.consume && done.notice.contains("loading"));
    assert!(!done.notice.contains("Unknown command"));
    client.pump()?;
    let done = run(&mut client, "/missing")?;
    assert!(done.notice.contains("Unknown command"));
    assert!(done.notice.contains("//missing"));
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

fn fresh_draft() -> TestResult<Client<Peer>> {
    let mut client = prepared(false)?;
    client.offer_directory("/work/project-a")?;
    client.begin_directory_draft()?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client.models().first().cloned().ok_or("model")?;
    client.select_model(&model)?;
    client.enable_commands();
    client.pump()?;
    Ok(client)
}

#[test]
fn first_prompt_commands_use_the_existing_draft_creation_owner_once() -> TestResult {
    for (text, expected) in [
        ("//literal first", "/literal first"),
        ("/review feature", "Review feature"),
        ("/init", INIT_PROMPT.trim_end()),
    ] {
        let mut client = fresh_draft()?;
        assert!(client.selected().is_none());
        assert_eq!(client.peer_mut().inner.session_count(), 0);
        let mut done = run(&mut client, text)?;
        assert!(done.consume, "{text}: {}", done.notice);
        assert!(
            done.keep_for(client.selected()),
            "new session lost command completion"
        );
        assert_eq!(client.peer_mut().inner.session_count(), 1);
        assert_eq!(client.peer_mut().prompts, [expected]);
        client.pump()?;
        assert!(run(&mut client, "//second")?.consume);
        assert_eq!(client.peer_mut().inner.session_count(), 1);
    }
    Ok(())
}

#[test]
fn first_prompt_local_refusals_create_no_session_and_send_no_model_input() -> TestResult {
    let mut client = fresh_draft()?;
    for text in [
        "/unknown",
        "/focus one",
        "/special",
        "/compact extra",
        "/rename",
        "/rewind",
    ] {
        let done = run(&mut client, text)?;
        assert!(!done.consume, "{text}");
        assert!(client.selected().is_none());
        assert_eq!(client.peer_mut().inner.session_count(), 0);
    }
    assert!(client.peer_mut().prompts.is_empty());
    assert!(!client.peer_mut().calls.contains(&Operation::SessionCreate));
    Ok(())
}

#[test]
fn refused_expansions_starting_with_slashes_retry_as_exact_prompt_text() -> TestResult {
    let mut client = fresh_draft()?;
    client.peer_mut().refuse_prompt = true;
    let done = run(&mut client, "/slash-task details")?;
    assert!(!done.consume);
    assert_eq!(done.replacement.as_deref(), Some("  //work details"));
    assert_eq!(client.peer_mut().inner.session_count(), 0);
    client.peer_mut().refuse_prompt = false;
    assert!(
        run(
            &mut client,
            done.replacement.as_deref().ok_or("expanded retry")?
        )?
        .consume
    );
    assert_eq!(client.peer_mut().prompts, ["  /work details"]);
    assert_eq!(client.peer_mut().inner.session_count(), 1);
    Ok(())
}

mod selection;

mod permissions;
mod workspace_guidance;

#[test]
fn custom_preview_is_explicit_and_preserves_the_exact_task_without_a_session() -> TestResult {
    let mut client = fresh_draft()?;
    let before = client.peer_mut().calls.len();
    let mut done = run(&mut client, "/focus first second")?;
    assert!(!done.consume);
    assert!(done.keep_for(client.selected()));
    assert!(client.selected().is_none());
    assert!(!client.peer_mut().calls[before..]
        .iter()
        .any(|op| matches!(op, Operation::SessionCreate | Operation::SessionPrompt)));
    let Some(View::Text(rows)) = &done.view else {
        return Err("missing command preview".into());
    };
    assert!(rows
        .iter()
        .any(|row| row.contains("/home/example/config/grapt.json")));
    assert!(rows.iter().any(|row| row == "Read first then second"));
    let task = done.replacement.take().ok_or("no staged task")?;
    client.refresh_commands();
    client.pump()?;
    submit(&mut client, &task, &mut done, false);
    assert!(done.consume);
    assert_eq!(client.peer_mut().prompts, ["Read first then second"]);
    Ok(())
}

#[test]
fn untrusted_project_command_is_listed_with_reason_and_cannot_create_or_send() -> TestResult {
    let mut client = fresh_draft()?;
    let result = run(&mut client, "/blocked-project")?;
    assert!(!result.consume && result.replacement.is_none());
    assert!(result.notice.contains("Repository is not trusted"));
    assert!(client.selected().is_none());
    assert!(client.peer_mut().prompts.is_empty());
    assert_eq!(client.peer_mut().inner.session_count(), 0);
    let Some(View::Text(rows)) = run(&mut client, "/commands")?.view else {
        return Err("list absent".into());
    };
    assert!(rows
        .iter()
        .any(|r| r.contains("/blocked-project unavailable: Repository is not trusted")));
    Ok(())
}

#[test]
fn an_incomplete_preview_cannot_stage_a_task_for_confirmation() -> TestResult {
    let mut client = fresh_draft()?;
    client.peer_mut().large_template = true;
    client.refresh_commands();
    client.pump()?;
    let done = run(&mut client, "/focus")?;
    assert!(!done.consume && done.replacement.is_none());
    assert!(done.notice.contains("complete preview bound"));
    assert!(client.peer_mut().prompts.is_empty());
    assert!(client.selected().is_none());
    Ok(())
}

#[test]
fn clear_starts_fresh_without_deleting_saved_input_or_changing_choices() -> TestResult {
    let mut client = prepared(true)?;
    let first = client.selected().cloned().ok_or("original session")?;
    let model = client.model().cloned();
    let agent = client.agent().cloned();
    client.submit_prompt("Keep this saved conversation")?;
    let count = client.peer_mut().inner.session_count();
    let refused = run(&mut client, "/clear extra")?;
    assert!(!refused.consume && refused.replacement.is_none());
    assert!(matches!(refused.view, Some(View::Notice(_))));
    assert_eq!(client.peer_mut().inner.session_count(), count);
    assert_eq!(client.selected(), Some(&first));
    let cleared = run(&mut client, "/clear")?;
    assert!(cleared.consume);
    assert_ne!(client.selected(), Some(&first));
    assert_eq!(client.model().cloned(), model);
    assert_eq!(client.agent().cloned(), agent);
    assert_eq!(client.peer_mut().inner.user_inputs(&first).len(), 1);
    assert_eq!(
        client.peer_mut().prompts,
        vec!["Keep this saved conversation"]
    );
    assert!(run(&mut client, &format!("/resume {}", first.spelled()))?.consume);
    assert_eq!(client.selected(), Some(&first));
    assert_eq!(client.peer_mut().inner.user_inputs(&first).len(), 1);
    for invalid in ["/help extra", "/status extra", "/clearer"] {
        let done = run(&mut client, invalid)?;
        assert!(!done.consume && done.replacement.is_none(), "{invalid}");
        assert!(matches!(done.view, Some(View::Notice(_))), "{invalid}");
    }
    Ok(())
}

#[test]
fn mcp_browser_login_checks_its_pending_flow_and_logout_without_a_model_prompt() -> TestResult {
    let mut client = prepared(true)?;
    let session = client.selected().cloned();
    let started = run(&mut client, "/mcp login docs/server")?;
    assert!(started.consume);
    let Some(View::Text(lines)) = started.view else {
        return Err("sign-in instructions".into());
    };
    assert!(lines
        .iter()
        .any(|line| line.starts_with("https://example.invalid/authorize")));
    assert!(run(&mut client, "/mcp login docs/server")?.consume);
    assert!(run(&mut client, "/mcp logout docs/server")?.consume);
    assert!(run(&mut client, "/mcp login docs/server")?.consume);
    let actions: Vec<_> = client
        .peer_mut()
        .auth
        .iter()
        .filter_map(|body| body["action"].as_str())
        .collect();
    assert_eq!(actions, ["login", "status", "logout", "login"]);
    assert_eq!(client.selected(), session.as_ref());
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

mod mcp_auth;

mod side;
mod worktree;
