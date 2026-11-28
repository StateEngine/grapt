use super::*;
use crate::backend::first_prompt::Submissions;
use crate::backend::CommandId;

#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct WorktreePeer {
    inner: ContractDouble,
    requests: Vec<WireRequest>,
    reply: Option<WireResponse>,
    commit_before_reply: bool,
    lost: bool,
    release_create: bool,
    /// The status and body the attachment's snapshot read is refused with.
    history_refusal: Option<(u16, &'static [u8])>,
    /// Raise a form and an ask on the target Session right after its snapshot
    /// is served, so both are newer than the snapshot.
    raise_after_snapshot: bool,
    /// The target Session, once its snapshot was served.
    snapshot_served: Option<grapt_client::wire::SessionWireId>,
    held: Option<WireRequest>,
    release_reads: bool,
    nonblocking_only: bool,
    frame_polls: usize,
    interrupts: usize,
    cancellations: usize,
}
impl WorktreePeer {
    fn mutations(&self) -> usize {
        self.requests
            .iter()
            .filter(|request| {
                matches!(
                    request.operation(),
                    Operation::SessionCreate | Operation::WorkspaceWorktree
                )
            })
            .count()
    }
}
impl Transport for WorktreePeer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        assert!(
            !matches!(
                request.operation(),
                Operation::WorkspaceWorktree | Operation::SessionPrompt
            ),
            "worktree blocked the driver or submitted a model prompt"
        );
        assert!(
            !self.nonblocking_only || request.operation() == Operation::SessionInterrupt,
            "post-create attachment used Connection::call"
        );
        if request.operation() == Operation::SessionInterrupt {
            self.interrupts += 1;
        }
        self.inner.call(request)
    }
}
impl Connection for WorktreePeer {
    fn begin_operator(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        assert!(self.held.is_none(), "one operator request owner");
        assert!(self.requests.len() < 64, "fixture request bound");
        self.requests.push(request.clone());
        self.held = Some(request.clone());
        Ok(None)
    }
    fn poll_operator(&mut self) -> Result<Option<WireResponse>, TransportError> {
        if std::mem::take(&mut self.lost) {
            self.held = None;
            return Err(TransportError::AfterSend {
                reason: "lost worktree response".into(),
            });
        }
        if let Some(reply) = self.reply.take() {
            if std::mem::take(&mut self.commit_before_reply) {
                let request = self.held.as_ref().ok_or(TransportError::NotConnected)?;
                assert_eq!(request.operation(), Operation::SessionCreate);
                self.inner.call(request)?; // Commit succeeded; projection can still fail.
            }
            self.held = None;
            return Ok(Some(reply));
        }
        let operation = self
            .held
            .as_ref()
            .ok_or(TransportError::NotConnected)?
            .operation();
        if operation == Operation::SessionCreate && std::mem::take(&mut self.release_create) {
            let request = self.held.take().ok_or(TransportError::NotConnected)?;
            let response = self.inner.call(&request)?;
            let id = grapt_client::wire::decode_session(&response.body)
                .map_err(|_| TransportError::NotConnected)?
                .id;
            // Actual product application_port::session_create reads the committed
            // record and calls the same session_response as session_get;
            // session_info includes placement.directory and saved agent/model.
            // The older double's create body omits those fields: use its saved
            // record here, then cross the real public create-response DTO codec.
            let mut response =
                self.inner
                    .call(&WireRequest::new(Operation::SessionGet, Some(&id), None)?)?;
            let limits = grapt_client::wire::limits().map_err(|_| TransportError::NotConnected)?;
            let dto: grapt_api::u3a::SessionResponseDto =
                grapt_api::u3a::decode_page(&response.body, limits)
                    .map_err(|_| TransportError::NotConnected)?;
            response.body = grapt_api::u3a::encode_page(&dto, limits)
                .map_err(|_| TransportError::NotConnected)?;
            return Ok(Some(response));
        }
        if operation == Operation::WorkspaceWorktree || !self.release_reads {
            return Ok(None);
        }
        let request = self.held.take().ok_or(TransportError::NotConnected)?;
        if let Some((status, body)) = self
            .history_refusal
            .filter(|_| operation == Operation::SessionSnapshot)
        {
            return Ok(Some(WireResponse {
                status,
                body: body.to_vec(),
            }));
        }
        let response = self.inner.call(&request)?;
        if operation == Operation::SessionSnapshot {
            let session = request
                .path()
                .split('/')
                .find(|segment| segment.starts_with("ses_"))
                .and_then(|segment| grapt_client::wire::session_id(segment).ok())
                .ok_or(TransportError::NotConnected)?;
            if self.raise_after_snapshot {
                self.inner.raise_form(&session, "raised after the snapshot");
                self.inner
                    .ask_permission(&session, "asked after the snapshot");
            }
            self.snapshot_served = Some(session);
        }
        Ok(Some(response))
    }
    fn cancel_operator(&mut self) {
        self.reply = None;
        self.held = None;
        self.cancellations += 1;
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.frame_polls += 1;
        self.inner.poll_frame()
    }
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.inner.call(request).map(Some)
    }
    fn begin_catalog(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.inner.call(request).map(Some)
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
}

fn prepared() -> TestResult<Client<WorktreePeer>> {
    let mut client = Client::new(
        WorktreePeer {
            inner: ContractDouble::new(),
            requests: Vec::new(),
            reply: None,
            commit_before_reply: false,
            lost: false,
            release_create: false,
            history_refusal: None,
            raise_after_snapshot: false,
            snapshot_served: None,
            held: None,
            release_reads: false,
            nonblocking_only: false,
            frame_polls: 0,
            interrupts: 0,
            cancellations: 0,
        },
        Bounds::PRODUCTION,
    );
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
    client.create_session()?;
    // The subscription proves its mode and the session is read before a test
    // raises anything, as on a connected terminal.
    client.pump()?;
    Ok(client)
}

fn finish_attachment(
    submissions: &mut Submissions,
    client: &mut Client<WorktreePeer>,
) -> TestResult {
    for _ in 0..32 {
        client.pump()?;
        if submissions.advance(client).is_some() {
            return Ok(());
        }
    }
    Err("attachment did not settle within its fixed read phases".into())
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn worktree_commands_encode_paths_and_complete_on_the_operator_lane() -> TestResult {
    let mut client = prepared()?;
    for invalid in [
        "/worktree",
        "/worktree move /tmp/x",
        "/worktree create /tmp/x --copy not-json",
        "/worktree open \"unterminated",
        "/worktree open ../relative checkout",
    ] {
        let done = execute(&mut client, invalid).ok_or("command")?;
        assert!(!done.consume && done.prepared_prompt.is_none());
    }
    assert!(client.peer_mut().requests.is_empty());
    let source = client.form_location().ok_or("source")?;
    let session = client.selected().cloned().ok_or("session")?;
    let mut submissions = Submissions::default();
    submissions.submit(
        &mut client,
        r#"/worktree create "../review task" --copy [".env.local","config/local file"]"#.into(),
        false,
        CommandId::TEST,
    );
    assert!(client.worktree_pending() && client.operator_request_pending());
    assert!(client
        .begin_side_question("cannot replace worktree")
        .is_err());
    let request = client.peer_mut().requests.first().ok_or("request")?;
    assert_eq!(request.path(), "/api/workspace/worktree");
    assert!(request
        .query()
        .contains(&("session".into(), session.spelled().into())));
    let body: grapt_api::u3a::WorkspaceWorktreeRequestDto = grapt_api::u3a::decode_request(
        request.body().ok_or("body")?,
        grapt_client::wire::limits()?,
    )?;
    assert!(
        matches!(body, grapt_api::u3a::WorkspaceWorktreeRequestDto::Create { destination, setup_files }
        if destination.as_str() == "../review task" && setup_files[1].as_str() == "config/local file")
    );
    assert!(submissions.advance(&mut client).is_none());
    let created_directory = r#"/repo/"team"/review task"#;
    client.peer_mut().reply = Some(WireResponse {
        status: 200,
        body: serde_json::to_vec(&serde_json::json!({
            "location": {"directory":source.directory,"project":{"id":source.project.spelled(),"directory":source.directory}},
            "data":{"directory":created_directory,"message":"Checkout created","effect":"completed"}
        }))?,
    });
    assert!(submissions.advance(&mut client).is_some());
    let done = submissions
        .command
        .take()
        .map(|(_, done)| done)
        .ok_or("completion")?;
    assert!(done.consume && done.prepared_prompt.is_none());
    assert_eq!(client.selected(), Some(&session));
    assert!(!client.operator_request_pending());
    let Some(View::Text(lines)) = done.view else {
        return Err("create result".into());
    };
    let quoted_created = serde_json::to_string(created_directory)?;
    assert!(lines
        .iter()
        .any(|line| line.contains("Created backend cwd:") && line.contains(&quoted_created)));
    assert!(lines
        .iter()
        .any(|line| line.contains(&format!("/worktree open {quoted_created}"))));
    let quoted_source = serde_json::to_string(&source.directory)?;
    for (action, target) in [
        ("merge", "../review task"),
        ("remove", r#"/backend/team "quoted"/review task"#),
    ] {
        let quoted_target = serde_json::to_string(target)?;
        submissions.submit(
            &mut client,
            format!("/worktree {action} {quoted_target}"),
            false,
            CommandId::TEST,
        );
        client.peer_mut().reply = Some(WireResponse {
            status: 200,
            body: serde_json::to_vec(&serde_json::json!({
                "location":{"directory":source.directory,"project":{"id":source.project.spelled(),"directory":source.directory}},
                "data":{"directory":source.directory,"message":format!("{action} completed"),"effect":"completed"}
            }))?,
        });
        assert!(submissions.advance(&mut client).is_some());
        let done = submissions
            .command
            .take()
            .map(|(_, done)| done)
            .ok_or("management completion")?;
        assert!(done.consume);
        let Some(View::Text(lines)) = done.view else {
            return Err("management result".into());
        };
        assert!(lines
            .iter()
            .any(|line| line.contains("Source (backend cwd):") && line.contains(&quoted_source)));
        assert!(lines
            .iter()
            .any(|line| line.contains("Requested target:") && line.contains(&quoted_target)));
        assert!(lines
            .iter()
            .any(|line| line.contains("Relative targets") && line.contains("Source")));
        assert!(!lines
            .iter()
            .any(|line| line.contains("/worktree open") || line.contains("Created backend cwd:")));
        assert_eq!(client.selected(), Some(&session));
    }
    let before_partial = client.peer_mut().mutations();
    submissions.submit(
        &mut client,
        "/worktree create ../partial".into(),
        false,
        CommandId::TEST,
    );
    client.peer_mut().reply = Some(WireResponse {
        status: 200,
        body: serde_json::to_vec(&serde_json::json!({
            "location": {"directory":source.directory,"project":{"id":source.project.spelled(),"directory":source.directory}},
            "data":{"directory":source.directory,"message":"Setup failed; incomplete checkout preserved at ../partial","effect":"may_have_changed"}
        }))?,
    });
    assert!(submissions.advance(&mut client).is_some());
    let partial = submissions
        .command
        .take()
        .map(|(_, done)| done)
        .ok_or("setup refusal")?;
    assert!(!partial.consume && partial.notice.contains("incomplete checkout preserved"));
    assert!(partial.notice.contains("may have happened"));
    assert!(
        partial.notice.contains("Source (backend cwd):") && partial.notice.contains(&quoted_source)
    );
    assert!(partial.notice.contains("Requested target: \"../partial\""));
    assert!(!partial.notice.contains("(checkout:") && !partial.notice.contains("/worktree open"));
    assert!(submissions.advance(&mut client).is_none());
    let blocked = execute(&mut client, "/worktree create ../another").ok_or("partial retry")?;
    assert!(!blocked.consume && blocked.notice.contains("Requested target: \"../partial\""));
    assert_eq!(client.peer_mut().mutations(), before_partial + 1);
    Ok(())
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn worktree_failures_and_navigation_never_replay_mutations() -> TestResult {
    let mut client = prepared()?;
    let mut submissions = Submissions::default();
    let mut images = crate::images::Images::default();
    images.add(crate::images::prepared(
        "keep.png".into(),
        grapt_image::prepare_rgba(2, 2, vec![255; 16])?,
    )?)?;
    submissions.submit_image(
        &mut client,
        images.submission("/worktree merge ../review task".into())?,
        CommandId::TEST,
    );
    images.sent(CommandId::TEST);
    let (ticket, settlement) = submissions.image_result.take().ok_or("image settlement")?;
    assert!(!images.settle(ticket, settlement));
    assert_eq!(images.submission("kept".into())?.files.len(), 1);
    let source = client.form_location().ok_or("source")?;
    client.peer_mut().reply = Some(WireResponse {
        status: 200,
        body: serde_json::to_vec(&serde_json::json!({
            "location": {"directory":source.directory,"project":{"id":source.project.spelled(),"directory":source.directory}},
            "data":{"directory":source.directory,"message":"Dirty tree; inspect and clean before merging","effect":"not_started"}
        }))?,
    });
    assert!(submissions.advance(&mut client).is_some());
    let refused = submissions
        .command
        .take()
        .map(|(_, done)| done)
        .ok_or("definite refusal")?;
    assert!(!refused.consume && refused.notice.contains("Dirty tree"));
    assert!(refused
        .notice
        .contains("Requested target: \"../review task\""));
    assert!(!refused.notice.contains("(checkout:"));
    assert!(submissions.advance(&mut client).is_none());
    submissions.submit(
        &mut client,
        "/worktree merge ../review task".into(),
        false,
        CommandId::TEST,
    );
    assert!(client.worktree_pending());
    client.peer_mut().lost = true;
    assert!(submissions.advance(&mut client).is_some());
    let done = submissions
        .command
        .take()
        .map(|(_, done)| done)
        .ok_or("failure")?;
    assert!(!done.consume && done.notice.contains("may have happened"));
    let retry = execute(&mut client, "/worktree merge ../review task").ok_or("retry")?;
    assert!(!retry.consume);
    assert_eq!(client.peer_mut().requests.len(), 2);
    for status in [400, 408] {
        let mut client = prepared()?;
        let mut submissions = Submissions::default();
        submissions.submit(
            &mut client,
            "/worktree create ../timed-out".into(),
            false,
            CommandId::TEST,
        );
        client.peer_mut().reply = Some(WireResponse {
            status,
            body: grapt_client::wire::encode_invalid_request("Request refused or timed out")?,
        });
        assert!(submissions.advance(&mut client).is_some());
        let failure = submissions
            .command
            .take()
            .map(|(_, done)| done)
            .ok_or("generic error")?;
        assert!(!failure.consume && failure.notice.contains("may have happened"));
        assert!(
            !execute(&mut client, "/worktree create ../timed-out")
                .ok_or("generic error retry")?
                .consume
        );
        assert_eq!(client.peer_mut().requests.len(), 1);
    }
    for (effect, foreign_cwd, foreign_root) in [
        (Some("not_started"), true, false),
        (Some("not_started"), false, true),
        (Some("unrecognized"), false, false),
        (None, false, false),
    ] {
        let mut client = prepared()?;
        let source = client.form_location().ok_or("source")?;
        let mut submissions = Submissions::default();
        submissions.submit(
            &mut client,
            "/worktree merge ../unverified".into(),
            false,
            CommandId::TEST,
        );
        let directory = if foreign_cwd {
            format!("{}/foreign", source.directory)
        } else {
            source.directory.clone()
        };
        let root = if foreign_root {
            format!("{}/foreign", source.directory)
        } else {
            source.directory.clone()
        };
        let mut body = serde_json::json!({
            "location":{"directory":directory,"project":{"id":source.project.spelled(),"directory":root}},
            "data":{"directory":"/repo/unverified","message":"Do not infer admission from this receipt","effect":effect}
        });
        if effect.is_none() {
            body["data"].as_object_mut().ok_or("data")?.remove("effect");
        }
        client.peer_mut().reply = Some(WireResponse {
            status: 200,
            body: serde_json::to_vec(&body)?,
        });
        assert!(submissions.advance(&mut client).is_some());
        let done = submissions
            .command
            .take()
            .map(|(_, done)| done)
            .ok_or("unverified receipt")?;
        assert!(!done.consume && done.notice.contains("may have happened"));
        assert!(
            !execute(&mut client, "/worktree merge ../unverified")
                .ok_or("unverified retry")?
                .consume
        );
        assert_eq!(client.peer_mut().mutations(), 1);
    }
    let mut client = prepared()?;
    let source_session = client.selected().cloned().ok_or("source session")?;
    let source_directory = client.form_location().ok_or("source location")?.directory;
    let mut submissions = Submissions::default();
    submissions.submit(
        &mut client,
        "/worktree remove ../review task".into(),
        false,
        CommandId::TEST,
    );
    let body: serde_json::Value =
        serde_json::from_slice(client.peer_mut().requests[0].body().ok_or("body")?)?;
    assert_eq!(
        body,
        serde_json::json!({"action":"remove","directory":"../review task"})
    );
    client.create_session()?;
    let next_session = client.selected().cloned().ok_or("next session")?;
    assert!(!client.worktree_pending());
    assert!(submissions.advance(&mut client).is_some());
    let mut done = submissions
        .command
        .take()
        .map(|(_, done)| done)
        .ok_or("canceled")?;
    assert!(
        done.keep_for(client.selected()),
        "publication must preserve the safety notice"
    );
    assert!(!done.consume && done.notice.contains("may have happened"));
    assert!(done.replacement.is_none());
    // A second navigation can occur before the queued result is published.
    client.select(&source_session)?;
    assert!(!done.keep_for(client.selected()));
    assert!(!done.consume && done.replacement.is_none() && done.view.is_some());
    let quoted_source = serde_json::to_string(&source_directory)?;
    let Some(View::NoticeText { ref rows, .. }) = done.view else {
        return Err("wrapped failure details".into());
    };
    assert!(rows
        .iter()
        .any(|row| row.contains("Source (backend cwd):") && row.contains(&quoted_source)));
    assert!(rows
        .iter()
        .any(|row| row.contains("Requested target: \"../review task\"")));
    assert!(rows.iter().any(|row| row.contains("may have happened")));
    let (commands, received) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = std::sync::mpsc::sync_channel(1);
    let backend = crate::backend::Backend {
        commands,
        updates,
        wake,
    };
    let mut ui = crate::ui::Ui::new(false);
    ui.session_update(client.selected().cloned(), false);
    // The result is already published when input requests a newer selection.
    let mut published = Some(done);
    ui.select_view(&backend, next_session.clone())?;
    let (selection, crate::backend::Command::Select(_)) = received.try_envelope()? else {
        return Err("selection request".into());
    };
    let draft = "keep the edited draft while inspecting worktree details";
    ui.composer.insert_str(draft);
    ui.pending = Some(draft.into());
    let current = ui.accept_selection_update(None, std::slice::from_mut(&mut published));
    // Mirror main's fence: never apply the stale snapshot or settle its draft.
    if current {
        ui.session_update(Some(source_session), true);
        ui.publish_command_result(published.take());
    }
    assert!(!current && published.is_none() && ui.views.loading());
    assert!(!ui.running() && ui.command_pane.is_none() && ui.command_notice.is_none());
    assert_eq!(crate::keys::draft(&ui), draft);
    assert_eq!(ui.pending.as_deref(), Some(draft));
    let mut acknowledged_command = None;
    assert!(ui.accept_selection_update(
        Some(selection),
        std::slice::from_mut(&mut acknowledged_command),
    ));
    ui.usage_pane = Some(crate::usage::Pane::default());
    ui.session_update(Some(next_session), false);
    assert!(ui.publish_command_result(acknowledged_command));
    ui.status = "Connected".into(); // Same ordering as the backend update loop.
    assert!(
        ui.command_pane.is_some(),
        "failure details must use the wrapped/scrollable pane"
    );
    assert!(ui.command_notice.is_some());
    assert!(ui.usage_pane.is_none());
    assert!(!crate::recovery::rows(&ui, 40).is_empty());
    assert_eq!(crate::keys::draft(&ui), draft);
    assert_eq!(ui.pending.as_deref(), Some(draft));
    for code in [
        crossterm::event::KeyCode::End,
        crossterm::event::KeyCode::PageUp,
    ] {
        crate::keys::handle_key(
            crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
    }
    assert!(
        ui.command_pane.is_some() && ui.command_notice.is_some() && received.try_recv().is_err()
    );
    assert_eq!(crate::keys::draft(&ui), draft);
    assert!(
        !execute(&mut client, "/worktree remove ../review task")
            .ok_or("retry")?
            .consume
    );
    assert_eq!(client.peer_mut().requests.len(), 1);
    Ok(())
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn worktree_open_creates_a_new_session_and_keeps_its_completion() -> TestResult {
    let mut client = prepared()?;
    let original = client.selected().cloned().ok_or("original")?;
    let source_directory = client.project().ok_or("project")?.worktree.clone();
    client.offer_directory(&source_directory)?;
    let directory = client
        .projects()
        .iter()
        .find(|project| project.worktree != source_directory)
        .ok_or("other offered directory")?
        .worktree
        .clone();
    let mut submissions = Submissions::default();
    let text = format!("/worktree open {}", serde_json::to_string(&directory)?);
    submissions.submit(&mut client, text, false, CommandId::TEST);
    assert_eq!(client.selected(), Some(&original));
    let request = &client.peer_mut().requests[0];
    assert_eq!(request.operation(), Operation::SessionCreate);
    let body: serde_json::Value = serde_json::from_slice(request.body().ok_or("body")?)?;
    assert_eq!(body["location"]["directory"], directory);
    client.peer_mut().commit_before_reply = true;
    client.peer_mut().reply = Some(WireResponse {
        status: 400,
        body: grapt_client::wire::encode_invalid_request("Session directory refused")?,
    });
    assert!(submissions.advance(&mut client).is_some());
    let failure = submissions
        .command
        .take()
        .map(|(_, done)| done)
        .ok_or("creation refusal")?;
    assert!(!failure.consume && failure.notice.contains("may have happened"));
    assert!(
        !execute(&mut client, &format!("/worktree open {directory}"))
            .ok_or("unknown creation retry")?
            .consume
    );
    assert!(
        !execute(&mut client, "/new")
            .ok_or("unknown new retry")?
            .consume
    );
    assert_eq!(client.peer_mut().mutations(), 1);
    assert_eq!(client.peer_mut().inner.session_count(), 2);

    // Only an inspected/reopened client may start another explicit creation.
    let mut client = prepared()?;
    let original = client.selected().cloned().ok_or("reopened source")?;
    let source_directory = client.project().ok_or("reopened project")?.worktree.clone();
    client.offer_directory(&source_directory)?;
    let mut submissions = Submissions::default();
    let text = format!("/worktree open {}", serde_json::to_string(&directory)?);
    submissions.submit(&mut client, text, false, CommandId::TEST);
    client.peer_mut().nonblocking_only = true;
    client.peer_mut().release_create = true;
    assert!(submissions.advance(&mut client).is_none());
    assert!(client.worktree_pending() && client.operator_request_pending());
    assert!(submissions.advance(&mut client).is_none()); // Start the held ProjectCurrent GET.
    for _ in 0..3 {
        client.pump()?;
        assert!(submissions.advance(&mut client).is_none());
        assert_eq!(client.selected(), Some(&original));
        assert_eq!(
            client.form_location().ok_or("source location")?.directory,
            source_directory
        );
    }
    client.interrupt()?;
    assert!(client.peer_mut().frame_polls > 0 && client.peer_mut().interrupts > 0);
    assert!(client
        .begin_side_question("cannot replace attachment")
        .is_err());
    client.peer_mut().release_reads = true;
    for _ in 0..16 {
        if client
            .peer_mut()
            .held
            .as_ref()
            .is_some_and(|request| request.operation() == Operation::SessionSnapshot)
        {
            break;
        }
        client.pump()?;
        assert!(submissions.advance(&mut client).is_none());
    }
    assert!(client
        .peer_mut()
        .held
        .as_ref()
        .is_some_and(|request| request.operation() == Operation::SessionSnapshot));
    client.peer_mut().release_reads = false;
    let ask = client
        .peer_mut()
        .inner
        .ask_permission(&original, "new ask during attachment")
        .ok_or("new permission")?;
    let ask = grapt_client::wire::permission_id(&ask)?;
    client.peer_mut().nonblocking_only = false; // Explicit public authority refresh, not attachment I/O.
    client.refresh_permissions()?;
    client.peer_mut().nonblocking_only = true;
    assert!(client.permissions().iter().any(|row| row.request_id == ask));
    client.peer_mut().release_reads = true;
    finish_attachment(&mut submissions, &mut client)?;
    let mut done = submissions
        .command
        .take()
        .map(|(_, done)| done)
        .ok_or("open completion")?;
    assert!(done.consume && done.keep_for(client.selected()));
    assert_ne!(client.selected(), Some(&original));
    assert!(
        client.permissions().iter().any(|row| row.request_id == ask),
        "attachment replaced a newer global permission snapshot"
    );
    assert_eq!(
        client.project().ok_or("opened project")?.worktree,
        directory
    );
    assert_eq!(
        client.form_location().ok_or("opened location")?.directory,
        directory
    );
    client.peer_mut().nonblocking_only = false; // Ordinary resume is outside the attachment.
    client.resume_session(&original)?;
    assert_eq!(
        client.project().ok_or("original project")?.worktree,
        source_directory
    );
    assert_eq!(client.peer_mut().inner.session_count(), 2);
    assert!(submissions.advance(&mut client).is_none());
    assert_eq!(client.peer_mut().mutations(), 1);
    assert!(!client
        .peer_mut()
        .requests
        .iter()
        .any(|request| request.operation() == Operation::SessionGet));
    let mut client = prepared()?;
    let mut submissions = Submissions::default();
    let text = format!("/worktree open {}", serde_json::to_string(&directory)?);
    submissions.submit(&mut client, text.clone(), false, CommandId::TEST);
    client.peer_mut().lost = true;
    assert!(submissions.advance(&mut client).is_some());
    assert!(
        !submissions
            .command
            .take()
            .map(|(_, done)| done)
            .ok_or("lost creation")?
            .consume
    );
    assert!(!execute(&mut client, &text).ok_or("creation retry")?.consume);
    assert!(!execute(&mut client, "/new").ok_or("new retry")?.consume);
    assert_eq!(client.peer_mut().inner.session_count(), 1);
    assert_eq!(client.peer_mut().requests.len(), 1);
    let mut client = prepared()?;
    let source = client.selected().cloned();
    let mut submissions = Submissions::default();
    let text = format!("/worktree open {}", serde_json::to_string(&directory)?);
    submissions.submit(&mut client, text, false, CommandId::TEST);
    client.peer_mut().history_refusal = Some((
        400,
        br#"{"_tag":"InvalidRequestError","message":"History attachment failed"}"#,
    ));
    client.peer_mut().nonblocking_only = true;
    client.peer_mut().release_create = true;
    client.peer_mut().release_reads = true;
    assert!(submissions.advance(&mut client).is_none());
    finish_attachment(&mut submissions, &mut client)?;
    let mut done = submissions
        .command
        .take()
        .map(|(_, done)| done)
        .ok_or("partial attachment")?;
    assert!(!done.consume && done.keep_for(client.selected()));
    assert!(done.notice.contains("may have happened") && done.notice.contains("was created"));
    assert_eq!(client.selected(), source.as_ref());
    assert!(
        !execute(&mut client, &format!("/worktree open {directory}"))
            .ok_or("read failure retry")?
            .consume
    );
    assert_eq!(client.peer_mut().inner.session_count(), 2);
    assert_eq!(client.peer_mut().mutations(), 1);

    // A service too old for snapshots answers the read with a bare 404: the
    // attachment is refused naming the upgrade, and the source stays.
    let mut client = prepared()?;
    let source = client.selected().cloned();
    let mut submissions = Submissions::default();
    let text = format!("/worktree open {}", serde_json::to_string(&directory)?);
    submissions.submit(&mut client, text, false, CommandId::TEST);
    client.peer_mut().history_refusal = Some((404, b""));
    client.peer_mut().nonblocking_only = true;
    client.peer_mut().release_create = true;
    client.peer_mut().release_reads = true;
    assert!(submissions.advance(&mut client).is_none());
    finish_attachment(&mut submissions, &mut client)?;
    let done = submissions
        .command
        .take()
        .map(|(_, done)| done)
        .ok_or("too-old attachment")?;
    assert!(
        done.notice.contains("Upgrade the Grapt server"),
        "{}",
        done.notice
    );
    assert_eq!(client.selected(), source.as_ref());

    let mut client = prepared()?;
    let source = client.selected().cloned().ok_or("source")?;
    let ask = client
        .peer_mut()
        .inner
        .ask_permission(&source, "answer while attachment waits")
        .ok_or("ask")?;
    let ask = grapt_client::wire::permission_id(&ask)?;
    let mut submissions = Submissions::default();
    submissions.submit(
        &mut client,
        format!("/worktree open {directory}"),
        false,
        CommandId::TEST,
    );
    client.peer_mut().nonblocking_only = true;
    client.peer_mut().release_create = true;
    client.peer_mut().release_reads = true;
    assert!(submissions.advance(&mut client).is_none());
    for _ in 0..16 {
        if client
            .peer_mut()
            .held
            .as_ref()
            .is_some_and(|request| request.operation() == Operation::SessionSnapshot)
        {
            break;
        }
        client.pump()?;
        assert!(submissions.advance(&mut client).is_none());
    }
    assert!(client
        .peer_mut()
        .held
        .as_ref()
        .is_some_and(|request| request.operation() == Operation::SessionSnapshot));
    client.peer_mut().nonblocking_only = false; // Explicit send-only public API, not attachment I/O.
    client.send_permission_reply(
        &source,
        &ask,
        grapt_client::wire::PermissionReplyChoice::Once,
    )?;
    client.peer_mut().nonblocking_only = true;
    finish_attachment(&mut submissions, &mut client)?;
    assert!(
        submissions
            .command
            .take()
            .map(|(_, done)| done)
            .ok_or("answered-ask attachment")?
            .consume
    );
    assert!(
        !client.permissions().iter().any(|row| row.request_id == ask),
        "attachment resurrected an acknowledged send-only reply"
    );

    let mut client = prepared()?;
    let mut submissions = Submissions::default();
    submissions.submit(
        &mut client,
        format!("/worktree open {directory}"),
        false,
        CommandId::TEST,
    );
    client.peer_mut().nonblocking_only = true;
    client.peer_mut().release_create = true;
    assert!(submissions.advance(&mut client).is_none());
    assert!(submissions.advance(&mut client).is_none());
    client.pump()?;
    let before_cancel = client.peer_mut().cancellations;
    client.cancel_operator_request();
    assert!(client.peer_mut().cancellations > before_cancel);
    assert!(submissions.advance(&mut client).is_some());
    let done = submissions
        .command
        .take()
        .map(|(_, done)| done)
        .ok_or("canceled attachment")?;
    assert!(!done.consume && done.notice.contains("was created"));
    assert!(
        matches!(done.view, Some(View::NoticeText { ref rows, .. }) if rows.iter().any(|row| row.contains("may have happened")))
    );
    assert!(
        !execute(&mut client, &format!("/worktree open {directory}"))
            .ok_or("canceled retry")?
            .consume
    );
    assert!(
        !execute(&mut client, "/new")
            .ok_or("canceled creation retry")?
            .consume
    );
    assert_eq!(client.peer_mut().mutations(), 1);
    Ok(())
}

#[test]
fn a_staged_snapshot_never_overwrites_newer_forms_or_target_asks() -> TestResult {
    let mut client = prepared()?;
    let source_directory = client.project().ok_or("project")?.worktree.clone();
    client.offer_directory(&source_directory)?;
    let directory = client
        .projects()
        .iter()
        .find(|project| project.worktree != source_directory)
        .ok_or("other offered directory")?
        .worktree
        .clone();
    let mut submissions = Submissions::default();
    let text = format!("/worktree open {}", serde_json::to_string(&directory)?);
    submissions.submit(&mut client, text, false, CommandId::TEST);
    client.peer_mut().raise_after_snapshot = true;
    client.peer_mut().nonblocking_only = true;
    client.peer_mut().release_create = true;
    client.peer_mut().release_reads = true;
    for _ in 0..32 {
        if client.peer_mut().snapshot_served.is_some() {
            break;
        }
        client.pump()?;
        assert!(submissions.advance(&mut client).is_none());
    }
    let target = client
        .peer_mut()
        .snapshot_served
        .clone()
        .ok_or("snapshot never served")?;
    // A refresh accepted after the snapshot's read holds the newer ask.
    client.peer_mut().nonblocking_only = false;
    client.refresh_permissions()?;
    client.peer_mut().nonblocking_only = true;
    finish_attachment(&mut submissions, &mut client)?;
    assert_eq!(client.selected(), Some(&target));
    assert!(
        client
            .forms()
            .iter()
            .any(|form| form.title == "raised after the snapshot"),
        "the earlier snapshot overwrote the newer form list"
    );
    assert!(
        client
            .permissions()
            .iter()
            .any(|ask| ask.session_id == target),
        "the earlier snapshot overwrote a newer permission refresh"
    );
    Ok(())
}
