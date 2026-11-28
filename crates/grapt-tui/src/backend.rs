use crate::{bridge::Bridge, config::Config};
use grapt_client::{
    wire::{PermissionReplyChoice, SessionWireId, Spelled},
    Bounds, Client, PermissionAsk, SessionSummary, Slice,
};
use std::sync::{
    atomic::Ordering,
    mpsc::{self, Receiver, Sender, SyncSender},
};

pub(super) mod commands;
mod diff_labels;
mod family;
mod first_prompt;
mod followups;
pub use family::{Context, Preview};
mod forms;
mod idle;
mod import;
mod interrupt;
mod rows;
#[cfg(test)]
use idle::{wait, wake_delay};
mod publish;
mod reads;
pub use publish::{heartbeat, CommandId, Handoff, Link, LAG_AFTER};
mod view;
use view::Ledger;
pub use view::{Failure, Outcome, Settled, Update};
mod sessions;
use rows::RowSync;
mod activity;
use activity::{selected_activity, selected_running};
mod catalog;
mod command_lane;
mod setup;
pub(super) use catalog::CatalogRefresh;

#[derive(Debug)]
pub enum Command {
    Form(grapt_tui_forms::Request),
    SessionSearch(String),
    ManageSession(crate::sessions::Request),
    Submit(String),
    SubmitImages(crate::images::Prompt),
    Followup(crate::followups::Request),
    StartupPrompt(String),
    StartupRead,
    Retry(String),
    Select(Selection),
    Reply(PermissionAsk, PermissionReplyChoice),
    RevokeGrant(crate::grants::Request),
    /// Stops the selected Session. It carries the execution the operator saw
    /// running when they pressed the key, so a stop that lands after that
    /// execution ended cannot stop the next one; `None` stops whatever runs.
    Interrupt(Option<grapt_client::wire::ExecutionWireId>),
    /// As `Interrupt`, then resumes admitted input once the execution settles.
    InterruptAndResume(Option<grapt_client::wire::ExecutionWireId>),
    InterruptSession(grapt_client::wire::SessionWireId),
    Quit(SyncSender<Result<(), String>>),
    Models,
    RefreshModels,
    Commands,
    Roster,
    ChooseAgent(grapt_client::wire::AgentWireId),
    Model(grapt_client::wire::ModelRow),
    Browse(grapt_client::client::HistoryMove),
}

#[derive(Debug)]
pub struct Selection {
    pub id: SessionWireId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub display_name: Option<String>,
    pub context: Context,
    pub row: SessionSummary,
    pub status: String,
    pub current: bool,
}

pub struct Backend {
    /// Unbounded: only human-originated commands queue here. The one command
    /// the UI generates on its own, the model refresh, is a coalesced flag.
    pub commands: Sender<(CommandId, Command)>,
    pub updates: Handoff,
    pub wake: SyncSender<()>,
}

impl Backend {
    pub fn start(config: Config, ui_wake: SyncSender<()>) -> Result<Self, String> {
        let (commands, rx) = mpsc::channel();
        let updates = Handoff::default();
        let (wake, ready) = mpsc::sync_channel(1);
        let worker_wake = wake.clone();
        let handoff = updates.clone();
        let _worker = std::thread::Builder::new()
            .name("tui-r-protocol".into())
            .spawn(move || {
                if let Err(error) = run(config, rx, &handoff, ready, worker_wake, &ui_wake) {
                    handoff.fail(error);
                    let _ = ui_wake.try_send(());
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            commands,
            updates,
            wake,
        })
    }

    /// Take the pending view, waking the backend to build the next one.
    pub fn take_update(&self) -> Option<Result<Update, Failure>> {
        let taken = self.updates.take();
        if taken.is_some() {
            let _ = self.wake.try_send(());
        }
        taken
    }
}

fn run(
    config: Config,
    commands: Receiver<(CommandId, Command)>,
    updates: &Handoff,
    ready: Receiver<()>,
    wake: SyncSender<()>,
    ui_wake: &SyncSender<()>,
) -> Result<(), Failure> {
    let bounds = Bounds {
        max_retained_slices: 50_000,
        max_slice_bytes: 65_536,
        max_message_pages: 2_000,
        max_pending_events: 32,
        ..Bounds::PRODUCTION
    };
    let peer = Bridge::open(config.clone(), bounds, wake.clone()).map_err(|message| Failure {
        message,
        unattached: true,
    })?;
    serve(
        config,
        commands,
        updates,
        ready,
        wake,
        ui_wake,
        Client::new(peer, bounds),
    )
    .map_err(|message| Failure {
        message,
        unattached: false,
    })
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn serve(
    config: Config,
    commands: Receiver<(CommandId, Command)>,
    updates: &Handoff,
    ready: Receiver<()>,
    wake: SyncSender<()>,
    ui_wake: &SyncSender<()>,
    mut client: Client<Bridge>,
) -> Result<(), String> {
    client.enable_paged_history();
    client.set_catalog_profile(&config.root);
    client.set_launch_preset(config.preset);
    client.set_launch_effort(config.reasoning_effort.clone());
    let mut directory = config
        .directory
        .clone()
        .filter(|_| config.session.is_none())
        .map(setup::Directory::new);
    let attached = if directory.is_some() {
        Ok(crate::launch::Readiness::Unresolved)
    } else {
        (|| {
            if config.session.is_some() {
                // The saved-Session attachment reads permissions after this
                // fence. Events arriving during that read retain their hint.
                client
                    .peer_mut()
                    .authority_dirty
                    .swap(false, Ordering::AcqRel);
            }
            if let (Some(session), Some(directory)) = (&config.session, &config.directory) {
                client.offer_saved_directory(session, directory)?;
            }
            setup::attach(&mut client, config.session.as_ref())
        })()
    };
    let mut startup_retry = attached
        .as_ref()
        .err()
        .filter(|error| client.peer_mut().read_health.transient(error))
        .map(|_| tokio::time::Instant::now() + setup::RETRY_DELAY);
    let (mut startup, initial_status) = match attached {
        Ok(crate::launch::Readiness::Manual) if config.prompt.is_some() => (
            crate::launch::Readiness::Manual,
            "Seat startup paused; resolve saved choices or requests, then relaunch".to_owned(),
        ),
        Ok(crate::launch::Readiness::Loading) => (
            crate::launch::Readiness::Loading,
            setup::loading_status(config.session.is_some()).to_owned(),
        ),
        Ok(readiness) => (readiness, "Connected".to_owned()),
        Err(error) => (
            crate::launch::Readiness::Paused,
            setup::paused(&error, startup_retry.is_some()),
        ),
    };
    let mut roster = setup::roster(&client);
    let mut form_poll = forms::Poll::new(tokio::time::Instant::now());
    let mut form_results = Vec::new();
    let mut session_messages = Vec::new();
    let mut ledger = Ledger::default();
    let mut previous = None;
    let mut selection = None;
    let mut row_sync = RowSync::default();
    let mut reconciliations = (0, 0);
    let mut status = initial_status;
    let mut read_revision = 0;
    let mut permissions = reads::PermissionPoll::default();
    let mut connection_status = grapt_client::client::ConnectionStatus::Live;
    let mut submissions = first_prompt::Submissions::default();
    let mut followups = followups::Submissions::default();
    let mut imports = import::Import::default();
    let mut stops = interrupt::Stops::default();
    let mut navigation = None;
    let mut models = None;
    let mut models_requested = false;
    let mut connections_refresh = false;
    let mut model_result = None;
    let mut publish = true;
    let mut catalog = CatalogRefresh::default();
    let mut reads = reads::Reads::start(wake.clone())?;
    let mut lane = command_lane::Lane::default();
    let mut family = family::Family::default();
    let mut published = tokio::time::Instant::now() - crate::FRAME_INTERVAL;
    let _ = wake.try_send(());
    let mut backlog = false;
    loop {
        publish |= followups.reconcile(&client);
        ledger.collect(
            &mut submissions,
            &mut followups,
            &mut model_result,
            &mut form_results,
            &mut session_messages,
        );
        // Token/child events still pass the common validator. A protocol wake
        // alone does not request a UI frame when its projection did not change.
        // A view is built only into a vacant slot, so none is ever dropped;
        // the UI's take wakes this loop to build the next one.
        if publish && published.elapsed() >= crate::FRAME_INTERVAL && updates.vacant() {
            let generation = (
                if client.browsing_history() {
                    0
                } else {
                    client.diagnostics().reconciliations
                },
                client.history_revision(),
            );
            let selected = client.selected().cloned();
            for done in ledger.completions() {
                if !done.keep_for(selected.as_ref()) {
                    models = None;
                    status = done.notice.clone();
                }
            }
            let reconcile = selection.is_none()
                && previous == selected
                && generation.1 == reconciliations.1
                && generation.0 != reconciliations.0;
            let reset =
                selection.is_some() || previous != selected || generation != reconciliations;
            let rows = client
                .display_view()
                .map(|v| v.slices.as_slice())
                .unwrap_or_default();
            let mut delta = row_sync.diff(rows, reset);
            diff_labels::relative(
                delta
                    .rows
                    .iter_mut()
                    .chain(delta.patches.iter_mut().map(|(_, row)| row)),
                client.project(),
                client.form_location().as_ref(),
            );
            previous = selected.clone();
            reconciliations = generation;
            let context_pending = family.refresh(&mut client);
            let agents = client
                .sessions()
                .iter()
                .map(|row| {
                    let display_name = row.detail.agent.as_deref().and_then(|id| {
                        client
                            .agents()
                            .iter()
                            .find(|agent| grapt_client::wire::Spelled::spelled(&agent.id) == id)
                            .map(|agent| agent.name.clone())
                    });
                    let context = family.context(&client, row);
                    let current = Some(&row.id) == client.selected();
                    let status = family.status(&client, row);
                    let mut row = row.clone();
                    if let Some(name) = &display_name {
                        row.detail.agent = Some(name.clone());
                    }
                    Agent {
                        context,
                        current,
                        row,
                        status,
                        display_name,
                    }
                })
                .collect();
            let title = client
                .sessions()
                .iter()
                .find(|row| Some(&row.id) == selected.as_ref())
                .map(|row| row.title.clone())
                .unwrap_or_else(|| {
                    selected
                        .as_ref()
                        .map(|id| id.spelled().to_owned())
                        .unwrap_or_else(|| "New session".into())
                });
            let running = selected_running(&client);
            let streamed_tokens = client
                .view()
                .and_then(|view| view.streamed_text_bytes())
                .map(|bytes| bytes.div_ceil(4));
            let permission = client.permissions().iter().find(|ask| ask.live).cloned();
            let display_status = if status == "Connected" {
                client
                    .peer_mut()
                    .read_health
                    .message()
                    .unwrap_or_else(|| status.clone())
            } else {
                status.clone()
            };
            let link = Link::of(
                client.connection_status(),
                &client.peer_mut().connection().state,
            );
            grapt_tui_transcript::profile::update();
            updates.put(Update {
                selection: selection.take(),
                previews: family.take_previews(),
                queued: client
                    .view()
                    .map(|view| view.queued.clone())
                    .unwrap_or_default(),
                slash: crate::slash::Catalog::from_client(&client),
                forms: client
                    .forms()
                    .iter()
                    .chain(client.location_forms())
                    .cloned()
                    .collect(),
                form_location: client.form_location(),
                startup,
                prompt_ready: first_prompt::ready(&client, startup) && !submissions.preparing(),
                reset: delta.reset,
                reconcile,
                selected,
                running,
                streamed_tokens,
                connected: client.connection_status()
                    == grapt_client::client::ConnectionStatus::Live,
                link,
                observed_running: selected_activity(&client),
                execution: client.view().and_then(|view| match &view.execution {
                    grapt_client::ExecutionState::Running { execution_id } => {
                        Some(execution_id.clone())
                    }
                    _ => None,
                }),
                compacting: client
                    .selected()
                    .and_then(|id| client.observed_activity(id))
                    == Some(grapt_client::client::SessionActivity::Compacting),
                completion: client.view().and_then(|view| view.held_terminal.clone()),
                start: delta.start,
                rows: delta.rows,
                agents,
                catalog: client.catalog_reach(),
                permission,
                policy: client.preset_line(),
                bypass_permissions: client.bypass_permissions_on(),
                mail_attention: client.view().and_then(|view| view.mail_attention),
                continuity_attention: client.view().and_then(|view| view.continuity_attention),
                status: first_prompt::notice(&client, &display_status),
                // The client owns gap-recovery state: one flag, true from
                // the refused live frame until a re-read baseline installs.
                // Deriving it here from counters or cursor state re-announced
                // healed overflows on every later reconnect, and reached into
                // state the connection rework is replacing.
                stream_gap: client.gap_recovery_pending(),
                repository_status: directory
                    .as_ref()
                    .and_then(setup::Directory::notice)
                    .or_else(|| client.repository_status())
                    .map(str::to_owned),
                feedback: client.view().and_then(|view| view.feedback.clone()),
                failed_execution: client.view().and_then(|view| match &view.execution {
                    grapt_client::ExecutionState::Settled {
                        execution_id,
                        outcome: grapt_client::Outcome::Failed,
                    } => execution_id.clone(),
                    _ => None,
                }),
                usage: grapt_client::view::status::snapshot(&client),
                title,
                mcp_summary: grapt_client::view::mcp_summary_lines(&client, 120),
                mcp_detail: grapt_client::view::mcp_detail_lines(&client, 120),
                patches: delta.patches,
                history_edges: client.history_edges(),
                navigation: navigation.take(),
                browsing: client.browsing_history(),
                model: client.model().cloned(),
                models: models.take(),
                roster: roster.take(),
                outcomes: ledger.take(),
            })?;
            let _ = ui_wake.try_send(());
            published = tokio::time::Instant::now();
            publish = context_pending;
        }
        let paint = (publish && updates.vacant())
            .then(|| crate::FRAME_INTERVAL.saturating_sub(published.elapsed()));
        // Refreshes held behind a pending resume keep their due times but must
        // not wake the loop; the resume's own deadline and the socket still do.
        let held = client.resume_pending();
        idle::pause(
            (updates, ui_wake, &ready),
            backlog || lane.ready(),
            [
                startup.attached().then(|| client.recovery_wait()).flatten(),
                (startup.attached() && !held)
                    .then(|| catalog.wait(tokio::time::Instant::now()))
                    .flatten(),
                paint,
                permissions
                    .wait(tokio::time::Instant::now())
                    .filter(|_| !held),
                startup_retry.map(|at| at.saturating_duration_since(tokio::time::Instant::now())),
                directory.as_ref().and_then(setup::Directory::wait),
                (startup.attached() && !client.catalog_pending() && !held)
                    .then(|| form_poll.wait(tokio::time::Instant::now()))
                    .flatten(),
            ],
        )?;
        // Adjacent view requests have no effects between them: attach only the
        // newest one. Preserve ordering around submissions, replies and stops:
        // commands behind a reply or open wait for it, a Quit never does.
        let mut batch = Vec::new();
        if updates.refresh_requested() {
            batch.push(updates.envelope(Command::RefreshModels));
        }
        // Commands never wait for the UI to take a view: a full chunk leaves
        // a backlog, and the next pass begins without sleeping.
        backlog = lane.fill(&commands, updates);
        batch.extend(lane.next(updates));
        let mark = ledger.len();
        for (id, command) in batch {
            // Each command's results leave their holders before the next
            // command runs, so none can replace another's.
            ledger.collect(
                &mut submissions,
                &mut followups,
                &mut model_result,
                &mut form_results,
                &mut session_messages,
            );
            publish = true;
            submissions.cancel_for(&command);
            let literal_startup = matches!(&command, Command::StartupPrompt(_));
            let result = match command {
                Command::Followup(request) if stops.pending_for(Some(&request.session)) => {
                    followups.result = Some((id, crate::followups::Custody::Refused));
                    Err(interrupt::submission_refused())
                }
                Command::Followup(request) => {
                    let ready = first_prompt::ready(&client, startup) && !submissions.preparing();
                    followups.submit(&mut client, request, ready, id)
                }
                Command::SessionSearch(query) => {
                    session_messages.push((id, sessions::search(&mut client, query)));
                    Ok(())
                }
                Command::ManageSession(request) => {
                    session_messages.push((id, sessions::manage(&mut client, request)));
                    if client.selected().is_none() {
                        startup = crate::launch::Readiness::Manual;
                    }
                    Ok(())
                }
                Command::Form(request) => {
                    form_results.push((id, forms::execute(&mut client, request)));
                    Ok(())
                }
                Command::StartupRead => setup::retry(
                    &mut client,
                    &mut directory,
                    &mut startup,
                    config.session.as_ref(),
                    config.directory.as_deref(),
                )
                .map(|()| {
                    roster = setup::roster(&client);
                }),
                Command::SubmitImages(prompt)
                    if !startup.can_submit() || stops.pending_for(client.selected()) =>
                {
                    submissions.image_result = Some((id, crate::images::Settlement::Rejected));
                    submissions.outcome = Some((id, (prompt.text, false, true)));
                    Err(if stops.pending_for(client.selected()) {
                        interrupt::submission_refused()
                    } else {
                        grapt_client::ClientError::SelectionIncomplete
                    })
                }
                Command::SubmitImages(prompt) => {
                    let dispatched = submissions.submit_image(&mut client, prompt, id);
                    status = dispatched.notice;
                    if let Some(movement) = dispatched.navigation {
                        navigation = Some(movement);
                    }
                    if dispatched.models.is_some() {
                        models = dispatched.models;
                    }
                    continue;
                }
                Command::Submit(text) | Command::StartupPrompt(text)
                    if !startup.can_submit() || stops.pending_for(client.selected()) =>
                {
                    submissions.outcome = Some((id, (text, false, !literal_startup)));
                    Err(if stops.pending_for(client.selected()) {
                        interrupt::submission_refused()
                    } else {
                        grapt_client::ClientError::SelectionIncomplete
                    })
                }
                Command::Submit(text) if import::matches(&text) => {
                    let ready = client.peer_mut().connection().state.streaming()
                        && !selected_running(&client)
                        && client.catalog_validated()
                        && !client.operator_request_pending()
                        && !submissions.preparing();
                    let done = imports.start(&client, &config, text, wake.clone(), (ready, id));
                    status = done.notice.clone();
                    submissions.command = Some((id, done));
                    continue;
                }
                Command::Submit(text) | Command::StartupPrompt(text) => {
                    let dispatched = submissions.submit(&mut client, text, literal_startup, id);
                    status = dispatched.notice;
                    if let Some(movement) = dispatched.navigation {
                        navigation = Some(movement);
                    }
                    if dispatched.models.is_some() {
                        models = dispatched.models;
                    }
                    continue;
                }
                Command::Retry(_) if !startup.can_submit() => {
                    Err(grapt_client::ClientError::SelectionIncomplete)
                }
                Command::Retry(_) if stops.pending_for(client.selected()) => {
                    submissions.retry_result(client.selected(), false);
                    Err(interrupt::submission_refused())
                }
                Command::Retry(text) => {
                    let result = client.retry_prompt(&text);
                    submissions.retry_result(client.selected(), result.is_ok());
                    match result {
                        Ok(clear_human_draft) => {
                            submissions.outcome =
                                Some((id, (text, clear_human_draft, clear_human_draft)));
                            if clear_human_draft {
                                Ok(())
                            } else {
                                Err(grapt_client::ClientError::EarlierSubmitted)
                            }
                        }
                        Err(error) => {
                            submissions.outcome = Some((id, (text, false, !literal_startup)));
                            Err(error)
                        }
                    }
                }
                Command::Select(request) => {
                    navigation = None;
                    startup = crate::launch::Readiness::Manual;
                    roster = None;
                    let lanes = (&mut client, &mut reads);
                    let result = lane.open(lanes, &mut directory, id, &request.id);
                    if result.is_err() {
                        selection = Some(id);
                    }
                    result
                }
                Command::RevokeGrant(request) => {
                    let done = commands::revoke_grant(&mut client, request);
                    status = done.notice.clone();
                    submissions.command = Some((id, done));
                    continue;
                }
                Command::Reply(ask, choice) => lane.reply(&mut client, &mut reads, &ask, choice),
                command @ (Command::Interrupt(_)
                | Command::InterruptAndResume(_)
                | Command::InterruptSession(_)) => {
                    status = stops.dispatch(&mut client, command, None, &wake);
                    continue;
                }
                Command::Quit(reply) if imports.busy() => {
                    let _ = reply.try_send(Err(
                        "Import is still running; wait for its result before closing.".into(),
                    ));
                    continue;
                }
                Command::Quit(reply) => {
                    if selected_running(&client) || stops.pending_for(client.selected()) {
                        // Close means closed: whatever runs stops, unfenced.
                        status = stops.dispatch(
                            &mut client,
                            Command::Interrupt(None),
                            Some(reply),
                            &wake,
                        );
                    } else {
                        let _ = reply.try_send(Ok(()));
                        let _ = ui_wake.try_send(());
                    }
                    continue;
                }
                Command::RefreshModels => {
                    connections_refresh = true;
                    if matches!(
                        startup,
                        crate::launch::Readiness::Unresolved | crate::launch::Readiness::Paused
                    ) {
                        setup::retry(
                            &mut client,
                            &mut directory,
                            &mut startup,
                            config.session.as_ref(),
                            config.directory.as_deref(),
                        )
                    } else {
                        client.refresh_connection_catalog()
                    }
                }
                Command::Models => {
                    // Copy the bounded catalog only on an explicit user request.
                    models_requested = true;
                    Ok(())
                }
                Command::Commands => {
                    client.open_commands();
                    Ok(())
                }
                Command::Roster => {
                    roster = Some(client.agents().to_vec());
                    Ok(())
                }
                Command::ChooseAgent(agent) => match client.select_agent(&agent) {
                    Ok(()) => {
                        models = Some(client.models().to_vec());
                        Ok(())
                    }
                    Err(error) => {
                        roster = setup::roster(&client);
                        Err(error)
                    }
                },
                Command::Model(model) => {
                    let result = if startup.attached() {
                        client.select_model(&model)
                    } else {
                        Err(grapt_client::ClientError::SelectionIncomplete)
                    };
                    model_result = Some((id, result.is_ok()));
                    result
                }
                Command::Browse(movement) => {
                    let result = client.browse_history(movement);
                    navigation = Some((movement, matches!(result, Ok(true))));
                    result.map(|_| ())
                }
            };
            status = match result {
                Ok(()) => "Connected".into(),
                Err(e) if startup == crate::launch::Readiness::Paused => {
                    if client.peer_mut().read_health.transient(&e) {
                        startup_retry = Some(tokio::time::Instant::now() + setup::RETRY_DELAY);
                    }
                    setup::paused(&e, startup_retry.is_some())
                }
                Err(e) if client.peer_mut().read_health.transient(&e) => "Connected".into(),
                Err(e) => e.to_string(),
            };
        }
        ledger.collect(
            &mut submissions,
            &mut followups,
            &mut model_result,
            &mut form_results,
            &mut session_messages,
        );
        if let Some(note) = ledger.form_note_after(mark) {
            status = note;
        }
        if let Some(notice) = stops.advance() {
            status = notice;
            publish = true;
            let _ = ui_wake.try_send(());
            let _ = wake.try_send(());
        }
        if let Some(next) = directory
            .as_mut()
            .and_then(|directory| directory.advance(&mut client))
        {
            startup = next;
            status = if next == crate::launch::Readiness::Unresolved {
                "Connected; repository unresolved"
            } else if config.session.is_none() {
                "Loading models…; draft editable"
            } else {
                "Restoring saved choices; draft editable"
            }
            .to_owned();
            publish = true;
        }
        if startup != crate::launch::Readiness::Paused {
            startup_retry = None;
        }
        // A launch with nobody at the terminal must not wait for Ctrl+O when
        // the service was only slow to answer a read.
        if startup_retry.is_some_and(|at| at <= tokio::time::Instant::now()) {
            startup_retry = None;
            publish = true;
            match setup::retry(
                &mut client,
                &mut directory,
                &mut startup,
                config.session.as_ref(),
                config.directory.as_deref(),
            ) {
                Ok(()) => {
                    roster = setup::roster(&client);
                    status = "Connected".into();
                }
                Err(e) => {
                    if client.peer_mut().read_health.transient(&e) {
                        startup_retry = Some(tokio::time::Instant::now() + setup::RETRY_DELAY);
                    }
                    status = setup::paused(&e, startup_retry.is_some());
                }
            }
        }
        if !startup.attached() {
            continue;
        }
        match if client.catalog_pending() {
            Ok(false)
        } else {
            reads.begin_forms(&mut client, &mut form_poll, tokio::time::Instant::now())
        } {
            Ok(changed) => publish |= changed,
            Err(error) => {
                publish = true;
                if !client.peer_mut().read_health.transient(&error) {
                    status = format!("Questions unavailable; last drafts kept: {error}");
                }
            }
        }
        let decoded_before = client.diagnostics().frames_decoded;
        let pumped = client.pump();
        grapt_tui_transcript::profile::decoded(
            client
                .diagnostics()
                .frames_decoded
                .saturating_sub(decoded_before),
        );
        match pumped {
            Ok(true) => {
                publish = true;
                let _ = wake.try_send(());
            }
            Ok(false) => {}
            Err(e) => {
                publish = true;
                if !client.peer_mut().read_health.transient(&e) {
                    status = e.to_string();
                }
            }
        }
        // Periodic reads answered on their own connection apply here, between
        // pumps; the loop never waits for one.
        let polls = (&mut permissions, &mut catalog, &mut form_poll);
        let (settled, notice) =
            reads.settle(&mut client, polls, tokio::time::Instant::now(), &wake);
        publish |= settled;
        status = notice.unwrap_or(status);
        let answered = lane.settle(&mut client, &mut reads);
        publish |= answered.apply(&mut session_messages, &mut selection, &mut status);
        if !client.catalog_pending() {
            if models_requested {
                models = Some(client.models().to_vec());
                models_requested = false;
                publish = true;
            }
            if connections_refresh {
                roster = setup::roster(&client);
                connections_refresh = false;
                publish = true;
            }
        }
        if !submissions.preparing() || !stops.pending_for(client.selected()) {
            if let Some(notice) = submissions.advance(&mut client) {
                status = notice;
                publish = true;
            }
        }
        if let Some(notice) = imports.advance(&mut submissions) {
            status = notice;
            publish = true;
        }
        if imports.busy() && submissions.command.is_some() {
            let _ = wake.try_send(());
        }
        let resolved = (startup == crate::launch::Readiness::Loading)
            .then(|| crate::launch::readiness(&client))
            .filter(|readiness| *readiness != crate::launch::Readiness::Loading);
        if let Some(readiness) = resolved {
            startup = readiness;
            roster = setup::roster(&client);
            if status == setup::loading_status(config.session.is_some()) {
                status = if startup == crate::launch::Readiness::Ready {
                    "Connected".to_owned()
                } else {
                    client
                        .catalog_status()
                        .unwrap_or(if config.session.is_none() {
                            "Connected; choose agent and model"
                        } else {
                            "Saved choices unavailable; choose agent/model; draft kept"
                        })
                        .to_owned()
                };
            }
            publish = true;
        }
        let next_connection = client.connection_status();
        if next_connection != connection_status {
            connection_status = next_connection;
            publish = true;
            status = match connection_status {
                // A startup still loading keeps saying so until it resolves.
                grapt_client::client::ConnectionStatus::Live
                    if startup == crate::launch::Readiness::Loading =>
                {
                    setup::loading_status(config.session.is_some()).into()
                }
                grapt_client::client::ConnectionStatus::Live => "Connected".into(),
                _ => "Service reconnecting; draft kept".into(),
            };
        }
        // A pending resume starts no read: the refresh stays queued until
        // the first frame answers it.
        match permissions.refresh(&mut client, &mut reads, tokio::time::Instant::now()) {
            Ok(started) => publish |= started,
            Err(e) => {
                publish = true;
                status = e.to_string();
            }
        }
        match catalog.refresh(&mut client, &mut reads, tokio::time::Instant::now()) {
            Ok(true) => publish = true,
            Ok(false) => {}
            Err(e) => {
                publish = true;
                if !client.peer_mut().read_health.transient(&e) {
                    status = e.to_string();
                }
            }
        }
        let revision = client.peer_mut().read_health.revision();
        if revision != read_revision {
            read_revision = revision;
            publish = true;
        }
    }
}

#[cfg(test)]
mod tests;
