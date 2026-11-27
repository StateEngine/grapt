use super::*;
use crate::backend::CommandId;
use grapt_client::{
    transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse},
    Bounds, ContractDouble, Operation,
};
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct Peer {
    inner: ContractDouble,
    held: Option<WireRequest>,
    release: bool,
    hold_readback: bool,
    refuse_create: bool,
    refuse_policy: bool,
    failed_readback: Option<bool>,
    selections: usize,
    creates: usize,
    prompts: Vec<String>,
    startup_prompts: usize,
    lose_prompt_reply: bool,
    prompt_images: Vec<Vec<String>>,
    content_calls: usize,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if request.operation() == Operation::McpContent {
            self.content_calls += 1;
            let catalog =
                WireRequest::new_typed::<SessionWireId>(Operation::CommandInspect, None, None)
                    .map_err(|_| TransportError::NotConnected)?
                    .with_query("location[directory]", "/work/project-a");
            let response = self.inner.call(&catalog)?;
            let catalog: serde_json::Value =
                serde_json::from_slice(&response.body).map_err(|_| TransportError::NotConnected)?;
            return Ok(WireResponse {
                status: 200,
                body: serde_json::json!({"kind":"user_content", "location":catalog["location"],
                    "content":{"text":"MCP user context","files":[]}})
                .to_string()
                .into_bytes(),
            });
        }
        if request.operation() == Operation::SessionCreate {
            self.creates += 1;
            if self.refuse_create {
                return Ok(WireResponse {
                    status: 503,
                    body: Vec::new(),
                });
            }
        }
        if request.operation() == Operation::SessionPresetSelect {
            self.selections += 1;
        }
        if request.operation() == Operation::SessionPresetGet {
            if let Some(malformed) = self.failed_readback {
                return Ok(WireResponse {
                    status: if malformed { 200 } else { 503 },
                    body: if malformed {
                        b"{}".to_vec()
                    } else {
                        Vec::new()
                    },
                });
            }
        }
        if request.operation() == Operation::SessionPresetSelect && self.refuse_policy {
            return Ok(WireResponse {
                status: 403,
                body: Vec::new(),
            });
        }
        if request.operation() == Operation::SessionPrompt {
            assert!(self.prompts.len() < 8, "fixture prompt bound");
            let prompt = grapt_client::wire::decode_prompt(
                request.body().ok_or(TransportError::NotConnected)?,
            )
            .map_err(|_| TransportError::NotConnected)?;
            let body: serde_json::Value =
                serde_json::from_slice(request.body().ok_or(TransportError::NotConnected)?)
                    .map_err(|_| TransportError::NotConnected)?;
            if body
                .pointer("/metadata/origin")
                .and_then(serde_json::Value::as_str)
                == Some("seat-startup")
            {
                self.startup_prompts += 1;
            }
            self.prompt_images.push(
                body.get("files")
                    .and_then(serde_json::Value::as_array)
                    .map(|files| {
                        files
                            .iter()
                            .filter_map(|file| {
                                file.get("name")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_owned)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            );
            self.prompts.push(prompt);
        }
        let mut result = self.inner.call(request);
        if matches!(
            request.operation(),
            Operation::CommandList | Operation::CommandInspect
        ) {
            let response = result.as_mut().map_err(|_| TransportError::NotConnected)?;
            let mut body: serde_json::Value =
                serde_json::from_slice(&response.body).map_err(|_| TransportError::NotConnected)?;
            body["data"] = serde_json::json!([{"command":{"name":"review","description":"Review","template":"Review $ARGUMENTS"},"origin":"builtin","source":"Grapt built-in"},{"command":{"name":"mcp__book__review","description":"Review","template":""},"origin":"mcp","source":"MCP book","mcp":{"server":"book","name":"review","arguments":[]}}]);
            response.body = serde_json::to_vec(&body).map_err(|_| TransportError::NotConnected)?;
        }
        if request.operation() == Operation::SessionPrompt
            && std::mem::take(&mut self.lose_prompt_reply)
        {
            return Err(TransportError::AfterSend {
                reason: "fixture lost reply".into(),
            });
        }
        result
    }
}
impl Connection for Peer {
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn subscribed(&self) -> bool {
        true
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        Ok(FramePoll::Idle)
    }
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        if matches!(
            request.operation(),
            Operation::SessionPresetGet | Operation::SessionPresetSelect
        ) {
            assert!(self.held.is_none(), "one HTTP owner");
            self.held = Some(request.clone());
            Ok(None)
        } else {
            self.call(request).map(Some)
        }
    }
    fn poll_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        if !self.release
            || (self.hold_readback
                && self
                    .held
                    .as_ref()
                    .is_some_and(|request| request.operation() == Operation::SessionPresetGet))
        {
            return Ok(None);
        }
        let request = self.held.take().ok_or(TransportError::NotConnected)?;
        self.call(&request).map(Some)
    }
    fn cancel_read(&mut self) {
        self.held = None;
    }
    fn begin_operator(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        assert!(self.held.is_none(), "one HTTP owner");
        self.held = Some(request.clone());
        Ok(None)
    }
    fn poll_operator(&mut self) -> Result<Option<WireResponse>, TransportError> {
        if !self.release {
            return Ok(None);
        }
        let request = self.held.take().ok_or(TransportError::NotConnected)?;
        self.call(&request).map(Some)
    }
}

fn client() -> Result<Client<Peer>, Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            held: None,
            release: false,
            hold_readback: false,
            refuse_create: false,
            refuse_policy: false,
            failed_readback: None,
            selections: 0,
            creates: 0,
            prompts: Vec::new(),
            startup_prompts: 0,
            lose_prompt_reply: false,
            prompt_images: Vec::new(),
            content_calls: 0,
        },
        Bounds::PRODUCTION,
    );
    client.offer_directory("/work/project-a")?;
    super::super::setup::attach(&mut client, None)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client.models().first().ok_or("model")?.clone();
    client.select_model(&model)?;
    client.set_launch_preset(Some(grapt_client::preset::Preset::Author));
    Ok(client)
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
fn first_enter_waits_for_confirmed_policy_then_submits_exactly_once() -> TestResult {
    let mut client = client()?;
    let mut submissions = Submissions::default();
    submissions.submit(&mut client, "first task".into(), false, CommandId::TEST);
    assert!(submissions.preparing());
    assert_eq!(client.peer_mut().creates, 1);
    assert!(client.peer_mut().prompts.is_empty());
    for _ in 0..3 {
        client.pump()?;
        assert!(submissions.advance(&mut client).is_none());
    }
    client.peer_mut().release = true;
    client.peer_mut().hold_readback = true;
    for _ in 0..3 {
        client.pump()?;
        assert!(submissions.advance(&mut client).is_none());
    }
    assert!(client.peer_mut().prompts.is_empty());
    client.peer_mut().hold_readback = false;
    for _ in 0..4 {
        client.pump()?;
        submissions.advance(&mut client);
    }
    assert_eq!(
        submissions.outcome.clone().map(|(_, outcome)| outcome),
        Some(("first task".into(), true, true))
    );
    assert_eq!(client.peer_mut().prompts, ["first task"]);
    assert_eq!(client.peer_mut().creates, 1);
    client.enable_commands();
    client.refresh_commands();
    for _ in 0..4 {
        client.pump()?;
    }
    client.peer_mut().release = false;
    submissions.submit(
        &mut client,
        "/mcp__book__review".into(),
        false,
        CommandId::TEST,
    );
    assert!(client.mcp_content_pending());
    assert!(submissions.advance(&mut client).is_none());
    assert!(client.mcp_content_pending());
    assert!(submissions.command.is_none());
    assert_eq!(client.peer_mut().content_calls, 0);
    submissions.outcome.take().map(|(_, outcome)| outcome);
    submissions.submit(&mut client, "/mcp verbose".into(), false, CommandId::TEST);
    assert_eq!(
        submissions.outcome.take().map(|(_, outcome)| outcome),
        Some(("/mcp verbose".into(), false, true))
    );
    assert!(submissions.operator.is_some());
    assert!(client.mcp_content_pending());
    let file = serde_json::from_value(serde_json::json!({
        "uri":"data:image/png;base64,AA==", "name":"kept.png"
    }))?;
    submissions.submit_image(
        &mut client,
        crate::images::Prompt {
            text: "/mcps".into(),
            files: vec![file],
        },
        CommandId::TEST,
    );
    assert_eq!(
        submissions.outcome.take().map(|(_, outcome)| outcome),
        Some(("/mcps".into(), false, true))
    );
    assert_eq!(
        submissions.image_result.take(),
        Some((CommandId::TEST, Settlement::Rejected))
    );
    assert!(submissions.operator.is_some());
    assert!(client.mcp_content_pending());
    assert_eq!(client.peer_mut().content_calls, 0);
    client.peer_mut().release = true;
    submissions.advance(&mut client);
    assert_eq!(
        client.peer_mut().prompts.last().map(String::as_str),
        Some("MCP user context")
    );
    assert!(
        !submissions.preparing(),
        "no unrelated wake is required after content completes"
    );
    Ok(())
}

#[test]
fn literal_command_keeps_its_original_draft_completion_during_preparation() -> TestResult {
    let mut client = client()?;
    let mut submissions = Submissions::default();
    submissions.submit(&mut client, "//literal".into(), false, CommandId::TEST);
    assert!(submissions.preparing());
    assert!(submissions.command.is_none());
    client.peer_mut().release = true;
    for _ in 0..4 {
        client.pump()?;
        submissions.advance(&mut client);
    }
    let (_, done) = submissions.command.ok_or("command result")?;
    assert!(done.consume);
    assert_eq!(done.text, "//literal");
    assert_eq!(client.peer_mut().prompts, ["/literal"]);
    Ok(())
}

#[test]
fn refused_creation_or_policy_retains_the_draft_without_a_prompt_retry() -> TestResult {
    for creation in [true, false] {
        let mut client = client()?;
        client.peer_mut().refuse_create = creation;
        client.peer_mut().refuse_policy = !creation;
        let mut submissions = Submissions::default();
        submissions.submit(&mut client, "retained task".into(), false, CommandId::TEST);
        client.peer_mut().release = true;
        for _ in 0..4 {
            client.pump()?;
            submissions.advance(&mut client);
        }
        assert_eq!(
            submissions.outcome.clone().map(|(_, outcome)| outcome),
            Some(("retained task".into(), false, true))
        );
        assert!(!submissions.preparing());
        assert!(client.peer_mut().prompts.is_empty());
        assert_eq!(client.peer_mut().creates, 1);
    }
    Ok(())
}

#[test]
fn session_navigation_cancels_preparation_without_sending_the_old_prompt() -> TestResult {
    let mut client = client()?;
    let mut submissions = Submissions::default();
    submissions.submit(&mut client, "old task".into(), false, CommandId::TEST);
    let first = client.selected().cloned().ok_or("first Session")?;
    let other = client.create_session()?;
    assert_ne!(first, other);
    submissions.advance(&mut client);
    assert_eq!(
        submissions.outcome.clone().map(|(_, outcome)| outcome),
        Some(("old task".into(), false, true))
    );
    assert!(!submissions.preparing());
    assert!(client.peer_mut().prompts.is_empty());
    Ok(())
}

#[test]
fn failed_or_malformed_policy_readback_keeps_the_first_draft_unsent() -> TestResult {
    for malformed in [false, true] {
        let mut client = client()?;
        let mut submissions = Submissions::default();
        submissions.submit(&mut client, "readback task".into(), false, CommandId::TEST);
        client.peer_mut().failed_readback = Some(malformed);
        client.peer_mut().release = true;
        let mut failure = None;
        for _ in 0..4 {
            client.pump()?;
            if let Some(notice) = submissions.advance(&mut client) {
                failure = Some(notice);
            }
        }
        assert_eq!(client.peer_mut().selections, 1);
        assert_eq!(client.peer_mut().creates, 1);
        assert_eq!(
            submissions.outcome.clone().map(|(_, outcome)| outcome),
            Some(("readback task".into(), false, true))
        );
        assert!(!submissions.preparing());
        assert!(client.peer_mut().prompts.is_empty());
        let notice = failure.ok_or("readback failure notice")?;
        assert_ne!(notice, "Connected");
        assert!(!ready(&client, crate::launch::Readiness::Ready));
        assert!(crate::launch::submit(&mut client, "readback task").is_err());
        assert!(client.peer_mut().prompts.is_empty());
    }
    Ok(())
}

#[test]
fn waiting_startup_keeps_its_origin_and_never_consumes_the_human_editor() -> TestResult {
    let mut client = client()?;
    let mut submissions = Submissions::default();
    submissions.submit(
        &mut client,
        "automated startup".into(),
        true,
        CommandId::TEST,
    );
    assert!(submissions.preparing());
    client.peer_mut().release = true;
    for _ in 0..4 {
        client.pump()?;
        submissions.advance(&mut client);
    }
    assert_eq!(
        submissions.outcome.clone().map(|(_, outcome)| outcome),
        Some(("automated startup".into(), true, false))
    );
    assert_eq!(client.peer_mut().prompts, ["automated startup"]);
    assert_eq!(client.peer_mut().startup_prompts, 1);
    Ok(())
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn uncertain_image_retry_settles_only_the_original_images_after_an_edited_enter() -> TestResult {
    use crate::images::{prepared, Images};
    let mut client = client()?;
    let mut submissions = Submissions::default();
    let mut images = Images::default();
    let image = |name: &str| prepared(name.into(), grapt_image::prepare_rgba(2, 2, vec![255; 16])?);
    images.add(image("original.png")?)?;
    client.peer_mut().lose_prompt_reply = true;
    submissions.submit_image(
        &mut client,
        images.submission("original".into())?,
        CommandId::TEST,
    );
    images.sent(CommandId::TEST);
    client.peer_mut().release = true;
    for _ in 0..4 {
        client.pump()?;
        submissions.advance(&mut client);
    }
    assert!(client.has_unconfirmed_prompt());
    let (ticket, result) = submissions
        .image_result
        .take()
        .ok_or("uncertain image outcome")?;
    assert!(!images.settle(ticket, result));
    let session_a = client.selected().cloned().ok_or("session A")?;
    images.add(image("new.png")?)?;
    submissions.submit_image(
        &mut client,
        images.submission("edited".into())?,
        CommandId::TEST,
    );
    images.sent(CommandId::TEST);
    let (ticket, result) = submissions
        .image_result
        .take()
        .ok_or("rejected edit outcome")?;
    assert!(!images.settle(ticket, result));
    assert_eq!(client.peer_mut().prompts, ["original"]);
    for command in ["/status", "/new"] {
        submissions.submit_image(
            &mut client,
            images.submission(command.into())?,
            CommandId::TEST,
        );
        images.sent(CommandId::TEST);
        assert!(
            submissions
                .command
                .take()
                .map(|(_, done)| done)
                .ok_or("read-only command")?
                .consume
        );
        let (ticket, result) = submissions
            .image_result
            .take()
            .ok_or("command image outcome")?;
        assert!(!images.settle(ticket, result));
    }
    assert_ne!(client.selected(), Some(&session_a));
    client.peer_mut().lose_prompt_reply = true;
    submissions.submit_image(
        &mut client,
        images.submission("session B".into())?,
        CommandId::TEST,
    );
    images.sent(CommandId::TEST);
    let (ticket, result) = submissions
        .image_result
        .take()
        .ok_or("conflicting session outcome")?;
    assert!(!images.settle(ticket, result));
    assert_eq!(client.peer_mut().prompts, ["original"]);
    assert!(!client.has_unconfirmed_prompt());
    client.peer_mut().lose_prompt_reply = false;
    client.resume_session(&session_a)?;
    for _ in 0..4 {
        client.pump()?;
    }
    let retry = client.retry_prompt("edited");
    assert_eq!(retry, Ok(false));
    submissions.retry_result(client.selected(), retry.is_ok());
    let (ticket, result) = submissions
        .image_result
        .take()
        .ok_or("retry image outcome")?;
    assert!(images.settle(ticket, result));
    assert_eq!(client.peer_mut().prompts, ["original", "original"]);
    let remaining = images.submission("edited".into())?;
    assert_eq!(remaining.files.len(), 1);
    assert_eq!(
        serde_json::to_value(&remaining.files)?[0]["name"],
        "new.png"
    );
    client.enable_commands();
    client.refresh_commands();
    for _ in 0..4 {
        client.pump()?;
    }
    for command in ["//literal", "/init", "/review changes"] {
        submissions.submit_image(
            &mut client,
            images.submission(command.into())?,
            CommandId::TEST,
        );
        images.sent(CommandId::TEST);
        for _ in 0..4 {
            client.pump()?;
            submissions.advance(&mut client);
        }
        let (ticket, result) = submissions
            .image_result
            .take()
            .ok_or("slash image outcome")?;
        assert!(images.settle(ticket, result), "{command}");
        let completion = submissions
            .command
            .take()
            .map(|(_, done)| done)
            .ok_or("slash completion")?;
        assert!(completion.consume);
        assert!(completion.replacement.is_none());
        assert_eq!(completion.session.as_ref(), client.selected());
        assert_eq!(
            client.peer_mut().prompt_images.last(),
            Some(&vec!["new.png".to_string()])
        );
        images.add(image("new.png")?)?;
    }
    for command in ["/mcp__book__review", "/mcp read book docs://guide"] {
        submissions.submit_image(
            &mut client,
            images.submission(command.into())?,
            CommandId::TEST,
        );
        images.sent(CommandId::TEST);
        let (ticket, result) = submissions.image_result.take().ok_or("MCP image outcome")?;
        assert!(!images.settle(ticket, result));
        let done = submissions
            .command
            .take()
            .map(|(_, done)| done)
            .ok_or("MCP refusal")?;
        assert!(!done.consume);
        assert!(!client.operator_request_pending());
        assert_eq!(client.peer_mut().content_calls, 0);
        assert_eq!(images.submission("kept".into())?.files.len(), 1);
    }
    Ok(())
}
