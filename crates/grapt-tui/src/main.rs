//! Candidate R: an isolated frontend; never an alternate engine or authority.
mod actions;
mod activity;
mod agents;
mod backend;
mod bridge;
mod catalog_gate;
mod chrome;
mod clipboard;
mod command_palette;
mod command_pane;
mod composer;
mod config;
mod connect;
mod editor;
mod followups;
mod gap_notice;
mod grants;
mod images;
mod input_bytes;
mod keybindings;
mod keys;
mod launch;
mod mail_status;
mod mentions;
mod models;
mod mouse;
mod navigation;
mod notify;
mod output;
mod pacing;
mod permission;
mod present;
mod quit;
mod read_health;
mod recall;
#[cfg(any(windows, test))]
mod record_input;
mod recovery;
mod selection;
#[cfg(test)]
mod selection_tests;
mod sending;
mod sessions;
mod settings;
mod slash;
mod startup_input;
mod subagents;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transcript_tests;
mod ui;
mod usage;
mod views;
#[cfg(windows)]
mod windows_input;

use backend::{Backend, Command};
use config::Config;
#[cfg(not(windows))]
use crossterm::event::EnableMouseCapture;
use crossterm::{
    event::{EnableBracketedPaste, EnableFocusChange, Event, KeyEventKind},
    execute,
    terminal::{self, BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen},
};
#[cfg(test)]
use keys::{draft, handle_key};
use keys::{edits_composer, history_request};
use ratatui::backend::CrosstermBackend;
use std::{io, sync::mpsc};
use ui::Ui;

use grapt_tui_transcript::prompt::MAX_DRAFT;
use pacing::INTERVAL as FRAME_INTERVAL;

fn main() -> std::process::ExitCode {
    let arguments: Vec<String> = match std::env::args_os()
        .skip(1)
        .map(std::ffi::OsString::into_string)
        .collect::<Result<_, _>>()
    {
        Ok(arguments) => arguments,
        Err(_) => {
            eprintln!("grapt-tui: launch arguments must be valid Unicode\nRun `grapt-tui --help` for usage.");
            return std::process::ExitCode::FAILURE;
        }
    };
    if arguments == ["--clipboard-read-image"] {
        return clipboard::read::image_helper();
    }
    #[cfg(unix)]
    if arguments == ["--clipboard-read-text"] {
        return clipboard::read::helper();
    }
    let mut preflight = arguments.iter();
    if let Some(option) = preflight.next() {
        if preflight.next().is_none() {
            match option.as_str() {
                "--help" | "-h" => {
                    println!("{}", config::HELP);
                    return std::process::ExitCode::SUCCESS;
                }
                "--version" | "-V" => {
                    println!("grapt-tui {}", env!("CARGO_PKG_VERSION"));
                    return std::process::ExitCode::SUCCESS;
                }
                _ => {}
            }
        }
    }
    let console = grapt_console::Access::default();
    match Config::parse_args(arguments).and_then(|config| run(config, &console)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(reason) => {
            // A startup refusal must reach the user synchronously, before the
            // process exits. A close that already began must not: its stderr may
            // be the full pipe that caused it, so that report never waits.
            if console.is_closing() {
                console.report_on_close(&reason);
            } else {
                console.report(io::stderr, &reason);
            }
            std::process::ExitCode::FAILURE
        }
    }
}

/// A lost terminal is a window close, whichever path noticed it.
///
/// The HTTP connection still works when the tty is gone, so the one thing that
/// must happen is the deliberate stop: the session must not be left running
/// until its silent-loss grace expires with nobody watching it. Every terminal
/// failure funnels here rather than returning out of the loop.
fn terminal_lost(
    ui: &mut Ui,
    backend: &Backend,
    access: &grapt_console::Access,
    elapsed: std::time::Duration,
    error: impl std::fmt::Display,
) {
    ui.status = format!("terminal is no longer writable: {error}");
    ui.launch.cancel();
    // The terminal is gone: restoration would only try to write to it, so the
    // close marks it unavailable before asking for the stop.
    access.close();
    ui.quit.close_console(backend, elapsed);
}

/// Asks for the deliberate stop if the loop unwinds without one.
///
/// A panic skips every explicit close, so the session would keep running until
/// its silent-loss grace expired. This guard sends the same quit request on
/// drop and is disarmed by a close that already ran.
struct PanicClose {
    commands: Option<(
        mpsc::Sender<(backend::CommandId, Command)>,
        backend::Handoff,
    )>,
}

impl PanicClose {
    fn new(backend: &Backend) -> Self {
        Self {
            commands: Some((backend.commands.clone(), backend.updates.clone())),
        }
    }

    /// A close has been asked for (or the loop is exiting normally): the guard
    /// has nothing left to add.
    fn disarm(&mut self) {
        self.commands = None;
    }
}

impl Drop for PanicClose {
    fn drop(&mut self) {
        if let Some((commands, handoff)) = self.commands.take() {
            // Nothing will read the acknowledgement: the process is unwinding.
            // The request itself is what the service needs to stop the work.
            let (reply, _result) = mpsc::sync_channel(1);
            let _ = commands.send(handoff.envelope(Command::Quit(reply)));
        }
    }
}

/// Hand a human-originated command to the backend. It never waits and never
/// drops: the intake is unbounded, and its depth shows with the footer's lag.
fn send(backend: &Backend, command: Command) -> Result<(), String> {
    send_tracked(backend, command).map(|_| ())
}

/// As `send`, returning the identity minted for the command, by which its
/// results are matched when they come back.
fn send_tracked(backend: &Backend, command: Command) -> Result<backend::CommandId, String> {
    backend.updates.handed();
    let (id, command) = backend.updates.envelope(command);
    if backend.commands.send((id, command)).is_err() {
        backend.updates.received();
        return Err("Service worker stopped; draft kept".to_owned());
    }
    let _ = backend.wake.try_send(());
    Ok(id)
}

/// Apply one view's results in the order they settled. For a session the UI
/// no longer shows, only what always applied there still does: submissions
/// settle, a safety notice is deferred, and session messages land.
fn deliver(ui: &mut Ui, settled: Vec<backend::Settled>, current: bool) -> bool {
    use backend::Outcome;
    let mut dirty = false;
    for backend::Settled { outcome, .. } in settled {
        dirty |= match outcome {
            Outcome::Submission(text, accepted, human) => {
                sending::submission(ui, (text, accepted, human), current)
            }
            Outcome::Command(done) if current => ui.publish_command_result(Some(done)),
            Outcome::Command(done) => ui.defer_safety(done),
            Outcome::Model(accepted) if current => {
                ui.model_update(ui.model.clone(), None, Some(accepted))
            }
            Outcome::Form(reply) if current => ui.forms.reply(reply),
            Outcome::Session(message) => {
                ui.session_message(message);
                true
            }
            Outcome::Model(_) | Outcome::Form(_) | Outcome::Followup(..) | Outcome::Image(..) => {
                false
            }
        };
    }
    dirty
}

/// A test's view of the command channel: each command without its id.
#[cfg(test)]
pub(crate) struct Commands(mpsc::Receiver<(backend::CommandId, Command)>);

#[cfg(test)]
impl Commands {
    pub(crate) fn try_recv(&self) -> Result<Command, mpsc::TryRecvError> {
        self.0.try_recv().map(|(_, command)| command)
    }
    pub(crate) fn try_iter(&self) -> impl Iterator<Item = Command> + '_ {
        self.0.try_iter().map(|(_, command)| command)
    }
    pub(crate) fn try_envelope(&self) -> Result<(backend::CommandId, Command), mpsc::TryRecvError> {
        self.0.try_recv()
    }
}

#[cfg(test)]
pub(crate) fn command_channel() -> (mpsc::Sender<(backend::CommandId, Command)>, Commands) {
    let (sender, receiver) = mpsc::channel();
    (sender, Commands(receiver))
}

/// A burst of commands larger than any queue the UI ever had.
#[cfg(test)]
fn burst(backend: &Backend) -> Result<(), String> {
    (0..1_000).try_for_each(|_| send(backend, Command::Models))
}

/// What reached the backend after a whole, in-order `burst`.
#[cfg(test)]
fn after_burst(received: &Commands) -> Result<Vec<Command>, String> {
    let mut burst: Vec<Command> = received.try_iter().collect();
    let rest = burst.split_off(burst.len().min(1_000));
    let whole = burst.len() == 1_000 && burst.iter().all(|c| matches!(c, Command::Models));
    whole
        .then_some(rest)
        .ok_or_else(|| "the burst did not arrive whole".into())
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn run(config: Config, access: &grapt_console::Access) -> Result<(), String> {
    let bindings = keybindings::Bindings::load(&config.root)?;
    let preferences = settings::Preferences::load(&config.root)?;
    let pins = sessions::Pins::load(&config.root)?;
    let mut capture =
        grapt_tui_transcript::profile::Capture::from_env().map_err(|e| e.to_string())?;
    let (wake, notification) = mpsc::sync_channel(1);
    let mut console = grapt_console::Listener::new(wake.clone())?;
    let backend = Backend::start(config.clone(), wake.clone())?;
    #[cfg(windows)]
    let _console_mode = windows_input::Mode::enter(access)?;
    terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    let _restore = grapt_console::Restore::new(access, io::stdout());
    execute!(
        io::stdout(),
        BeginSynchronizedUpdate,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableFocusChange,
        crossterm::style::Print("\x1b[?1003h\x1b[?1006h"),
        crossterm::style::Print(grapt_console::DISABLE_ALTERNATE_SCROLL),
        EndSynchronizedUpdate
    )
    .map_err(|e| e.to_string())?;
    // The Windows guard owns native mouse and VT input together. Crossterm's
    // mouse command replaces that mode and discards bracketed-paste boundaries.
    #[cfg(not(windows))]
    execute!(io::stdout(), EnableMouseCapture).map_err(|e| e.to_string())?;
    let mut input = startup_input::start(wake.clone(), config.prompt.is_some())?;
    let input_started = tokio::time::Instant::now();
    let mut notifier = notify::Notifier::new(config.notifications);
    notifier.set_enabled(preferences.values.notifications);
    let bytes = std::cell::Cell::new(0);
    let batch = output::Batch::terminal();
    let mut terminal = present::terminal(CrosstermBackend::new(output::Output::new(
        io::BufWriter::new(io::stdout()),
        &bytes,
        &batch,
    )))
    .map_err(|e| e.to_string())?;
    let mut ui = Ui::new(config.light);
    ui.bindings = bindings;
    ui.preferences = preferences;
    ui.session_pins = pins;
    ui.connect.start(config.clone(), wake.clone())?;
    ui.mentions.start(wake.clone())?;
    ui.launch.start(config.prompt);
    ui.transcript.set_wake(wake.clone());
    let mut presenter = present::Presenter::default();
    let mut clipboard = clipboard::Worker::start(wake.clone())?;
    let mut dirty = true;
    let mut refresh_connections = false;
    let mut last = tokio::time::Instant::now() - FRAME_INTERVAL;
    let mut typing_pause =
        pacing::TypingPause::new(std::env::var("GRAPT_R_TYPE_PAUSE_MS").ok().as_deref());
    // A panic in the loop must not skip the deliberate stop, or the session
    // runs on until its silent-loss grace expires with nobody watching. The
    // guard asks for the same close and is disarmed once a real one has run.
    let mut guard = PanicClose::new(&backend);
    loop {
        grapt_tui_transcript::profile::iteration();
        if let Some(event) = console.poll() {
            match event.intent() {
                grapt_console::Intent::Interrupt => {
                    quit::console_interrupt(&mut ui, &backend, input_started.elapsed());
                }
                grapt_console::Intent::Close => {
                    access.close();
                    ui.launch.cancel();
                    ui.quit.close_console(&backend, input_started.elapsed());
                }
            }
            dirty = true;
        }
        let (connections_changed, refresh_models) = ui.connect.poll();
        dirty |= connections_changed;
        if connections_changed {
            ui.clipboard.reset();
        }
        if let Some(notice) = ui.connect.notice.take() {
            ui.command_notice = Some(notice);
        }
        refresh_connections |= refresh_models;
        if refresh_connections {
            backend.updates.request_refresh();
            let _ = backend.wake.try_send(());
            refresh_connections = false;
        }
        let previously_dirty = dirty;
        let mut edited = false;
        let mut other_input = false;
        let mut input_drained = false;
        // Input has its own bounded queue and is always consumed before model data.
        for _ in 0..128 {
            let Ok(value) = input.try_recv() else {
                input_drained = true;
                break;
            };
            let _ = wake.try_send(());
            if ui.quit.closing() {
                continue;
            }
            if let Ok(event) = &value {
                notifier.set_enabled(ui.preferences.values.notifications);
                notifier.input(event, input_started.elapsed());
            }
            match value {
                Err(error) => {
                    ui.launch.cancel();
                    ui.status = error;
                    other_input = true;
                    dirty = true;
                }
                Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                    mouse::cancel(&mut ui);
                    if ui.selection.pressed() {
                        ui.selection.clear();
                        other_input = true;
                    }
                    if let Some(exit) = quit::key(key, &mut ui, &backend, input_started.elapsed()) {
                        ui.chords.cancel();
                        if exit {
                            if let Err(error) = ui.quit.request(&backend, input_started.elapsed()) {
                                ui.status = error;
                            }
                        }
                        other_input = true;
                        dirty = true;
                        continue;
                    }
                    let composer_key = !ui.connect.visible()
                        && ui.command_palette.is_none()
                        && ui.settings.is_none()
                        && ui.permission.is_none()
                        && ui.picker.is_none()
                        && ui.agent_picker.is_none()
                        && ui.models.is_none()
                        && ui.usage_pane.is_none()
                        && !ui.forms.focused
                        && ui.command_pane.is_none()
                        && ui.grants_pane.is_none()
                        && !ui.recall.searching()
                        && !ui.transcript.tools_focused()
                        && edits_composer(key);
                    edited |= composer_key;
                    other_input |= !composer_key;
                    if keys::handle_at(key, &mut ui, &backend, input_started.elapsed())? {
                        ui.launch.cancel();
                        if let Err(error) = ui.quit.request(&backend, input_started.elapsed()) {
                            ui.status = error;
                        }
                    }
                    dirty = true;
                }
                Ok(Event::Paste(text)) => {
                    ui.chords.cancel();
                    mouse::cancel(&mut ui);
                    if ui.quit.waiting() {
                        continue;
                    }
                    ui.quit.disarm();
                    if keys::paste(&text, &mut ui) {
                        edited = true;
                    } else {
                        other_input = true;
                    }
                    dirty = true;
                }
                Ok(Event::FocusLost) => {
                    ui.chords.cancel();
                    mouse::cancel(&mut ui);
                    ui.selection.clear();
                    ui.clipboard.reset();
                    other_input = true;
                    dirty = true;
                }
                Ok(Event::Resize(width, height)) => {
                    mouse::cancel(&mut ui);
                    ui.transcript.invalidate_hits();
                    ui.selection.clear();
                    presenter = present::Presenter::default();
                    if let Err(error) =
                        terminal.resize(ratatui::layout::Rect::new(0, 0, width, height))
                    {
                        terminal_lost(&mut ui, &backend, access, input_started.elapsed(), error);
                        continue;
                    }
                    dirty = true;
                    other_input = true;
                }
                Ok(Event::Mouse(event)) => {
                    if matches!(event.kind, crossterm::event::MouseEventKind::Down(_)) {
                        ui.chords.cancel();
                    }
                    if event.kind
                        == crossterm::event::MouseEventKind::Drag(
                            crossterm::event::MouseButton::Left,
                        )
                        && ui.transcript.dragging_scrollbar()
                    {
                        capture.pointer();
                    }
                    let grabbed = ui.transcript.dragging_scrollbar();
                    if mouse::handle(event, &mut ui) {
                        other_input = true;
                        dirty = true;
                    }
                    if !grabbed && ui.transcript.dragging_scrollbar() && ui.navigation.loading() {
                        history_request(
                            &mut ui,
                            &backend,
                            grapt_client::client::HistoryMove::Cancel,
                        );
                    }
                    presenter.capture_selection(&mut ui);
                    if let Some(action) = ui.switcher.action.take() {
                        subagents::perform(&mut ui, &backend, action);
                    }
                }
                _ => {}
            }
            dirty |= clipboard.poll(&mut ui, input_started.elapsed());
        }
        dirty |= clipboard.poll(&mut ui, input_started.elapsed());
        dirty |= ui.transcript.tick_compactions(input_started.elapsed());
        dirty |= sessions::poll_search(&mut ui, &backend, input_started.elapsed());
        let mention_enabled = ui.mentions_enabled();
        if !ui.quit.closing() && ui.launch.readiness.attached() {
            let draft = keys::draft(&ui);
            if let Err(error) = ui.slash.request_catalog(&draft, &backend) {
                ui.status = error;
                dirty = true;
            }
        }
        dirty |= ui.mentions.sync(&ui.composer, mention_enabled);
        dirty |= ui.mentions.poll();
        if other_input {
            typing_pause.cancel();
        }
        if edited && !other_input {
            // Echo before protocol work or the next transcript commit deadline.
            if let Err(error) =
                presenter.paint(&mut terminal, &mut ui, &mut capture, &bytes, &batch, true)
            {
                terminal_lost(&mut ui, &backend, access, input_started.elapsed(), error);
                continue;
            }
            typing_pause.edited(tokio::time::Instant::now());
            dirty = previously_dirty;
        }
        if ui.launch.waiting_to_send() {
            if let Err(error) = input.probe() {
                ui.launch.cancel();
                ui.status = error;
                dirty = true;
            }
            if input_drained && input.drained() {
                dirty |= launch::finish(&mut ui, &backend);
            }
        }
        if !ui.launch.has_intent() {
            input.retire();
        }
        // At most one protocol batch before input gets another opportunity.
        if let Some(update) = backend.take_update() {
            let _ = wake.try_send(());
            match update {
                Ok(mut update) => {
                    // Follow-up custody and image settlement write only their own
                    // drafts, which no other result reads, and land first as before.
                    let (early, settled): (Vec<_>, Vec<_>) = std::mem::take(&mut update.outcomes)
                        .into_iter()
                        .partition(|settled| {
                            matches!(
                                settled.outcome,
                                backend::Outcome::Followup(..) | backend::Outcome::Image(..)
                            )
                        });
                    for settled in early {
                        dirty |= match settled.outcome {
                            backend::Outcome::Followup(custody) => {
                                ui.followups.acknowledge(settled.id, custody)
                            }
                            backend::Outcome::Image(accepted) => {
                                ui.images.settle(settled.id, accepted)
                            }
                            _ => false,
                        };
                    }
                    notifier.set_enabled(ui.preferences.values.notifications);
                    access.with_output(|| notifier.update(&update, input_started.elapsed()));
                    for preview in update.previews {
                        ui.views.warm(preview);
                    }
                    let current = ui.accept_selection_update(update.selection, &mut []);
                    if !current {
                        dirty |= deliver(&mut ui, settled, false);
                    } else {
                        ui.sending.observe(update.selected.clone());
                        ui.followups.observe(
                            update.selected.clone(),
                            update.completion,
                            update.connected,
                            update.observed_running,
                        );
                        ui.link = update.link;
                        ui.launch.update(update.startup);
                        dirty |= ui.prompt_ready != update.prompt_ready;
                        ui.prompt_ready = update.prompt_ready;
                        dirty |= ui.workspace_update(
                            update
                                .form_location
                                .as_ref()
                                .map(|location| location.directory.as_str()),
                        );
                        dirty |= ui
                            .mentions
                            .workspace(update.form_location.as_ref().map(|l| l.directory.as_str()));
                        dirty |= ui.slash.catalog(update.slash);
                        if let Some(roster) = update.roster {
                            ui.agent_picker = Some(crate::agents::Picker::new(roster));
                        }
                        dirty |= ui.session_update(update.selected.clone(), update.running);
                        ui.live_execution = update.execution.clone();
                        dirty |= ui.queued != update.queued;
                        ui.queued = update.queued;
                        let obsolete_history = ui.navigation.obsolete_page(update.navigation);
                        dirty |= ui.transcript_update(
                            update.reset,
                            update.start,
                            update.rows,
                            update.reconcile,
                            update.navigation,
                        );
                        if !obsolete_history {
                            for (index, row) in update.patches {
                                dirty |= ui.transcript.update(false, index, vec![row]);
                            }
                        }
                        dirty |= ui.transcript.observe_running(update.running);
                        dirty |= ui.transcript.observe_compacting(update.compacting);
                        dirty |= ui.permission_update(update.permission);
                        dirty |= ui.policy != update.policy;
                        ui.policy = update.policy;
                        dirty |= ui.bypass_permissions != update.bypass_permissions;
                        ui.bypass_permissions = update.bypass_permissions;
                        dirty |= ui.mail_attention != update.mail_attention;
                        ui.mail_attention = update.mail_attention;
                        dirty |= ui.continuity_attention != update.continuity_attention;
                        ui.continuity_attention = update.continuity_attention;
                        dirty |= ui.stream_gap != update.stream_gap;
                        ui.stream_gap = update.stream_gap;
                        dirty |= ui.feedback != update.feedback;
                        ui.feedback = update.feedback;
                        ui.failed_execution = update.failed_execution;
                        dirty |= ui.usage != update.usage;
                        ui.usage = update.usage;
                        dirty |= ui.status != update.status
                            || ui.repository_status != update.repository_status
                            || ui.browsing != update.browsing
                            || ui.agents != update.agents
                            || ui.title != update.title
                            || ui.model != update.model;
                        ui.agents = update.agents;
                        dirty |= ui.catalog != update.catalog;
                        ui.catalog = update.catalog;
                        dirty |= ui.switcher.observe(&ui.agents, tokio::time::Instant::now());
                        ui.status = update.status;
                        ui.repository_status = update.repository_status;
                        // Update form state before observing activity so clearing a
                        // question form restores Thinking on the very first resumed update.
                        dirty |= ui.apply_forms_and_activity(
                            update.selected,
                            update.form_location,
                            update.forms,
                            update.running,
                            update.streamed_tokens,
                            tokio::time::Instant::now(),
                        );
                        // The other results apply in the order they settled, once
                        // the view's session state is installed; then any safety
                        // notice deferred from another session's view.
                        dirty |= deliver(&mut ui, settled, true);
                        dirty |= ui.publish_command_result(None);
                        // A completion opens the pane or picker these then fill.
                        dirty |= ui.mcp_update(update.mcp_summary, update.mcp_detail);
                        dirty |= ui.model_update(update.model, update.models, None);
                        ui.title = update.title;
                        if !obsolete_history {
                            ui.browsing = update.browsing;
                        }
                        dirty |= ui.restore_failed_input();
                        if !obsolete_history {
                            ui.navigation.edges = update.history_edges;
                        }
                        if let Some((movement, changed)) = update.navigation {
                            ui.navigation
                                .complete(movement, changed, &mut ui.transcript);
                            dirty = true;
                        }
                    }
                }
                Err(error) => {
                    ui.followups.disconnected();
                    ui.quit.backend_failed(error.unattached);
                    ui.status = error.message;
                    dirty = true;
                }
            }
        }
        let heartbeat = backend::heartbeat(
            ui.link,
            backend.updates.pass_age(),
            backend.updates.intake(),
        );
        dirty |= ui.heartbeat != heartbeat;
        ui.heartbeat = heartbeat;
        dirty |= sending::advance(&mut ui, &backend);
        dirty |= followups::advance(&mut ui, &backend);
        dirty |= clipboard.poll(&mut ui, input_started.elapsed());
        match ui.transcript.refresh_highlights() {
            Ok(changed) => dirty |= changed,
            Err(error) => {
                ui.status = error;
                dirty = true;
            }
        }
        let now = tokio::time::Instant::now();
        let binding_context = keybindings::context(&ui);
        dirty |= ui.chords.expire(
            binding_context,
            now.duration_since(input_started),
            &ui.bindings,
        );
        dirty |= ui.quit.tick(now.duration_since(input_started));
        if let Some(stopped) = ui.quit.poll(&backend, now.duration_since(input_started)) {
            if ui.quit.closing() {
                // The close is over whichever way it went. A timeout is reported
                // and the process exits anyway: the service's own close grace
                // covers the rare stop that never acknowledged, and hanging here
                // would be worse than a bounded uncertain exit.
                if let Err(reason) = &stopped {
                    // Never a direct write: a full stderr pipe would block the
                    // exit this close is trying to reach.
                    access.report_on_close(&format!(
                        "terminal closed without a confirmed stop: {reason}"
                    ));
                }
                guard.disarm();
                // Ratatui Drop can show the cursor; BufWriter Drop can flush.
                // This one terminal is abandoned only on imminent process exit.
                std::mem::forget(terminal);
                return stopped;
            }
            match stopped {
                Ok(()) => {
                    guard.disarm();
                    return Ok(());
                }
                Err(error) => {
                    ui.status = error;
                    dirty = true;
                }
            }
        }
        if mouse::tick(&mut ui, now.duration_since(input_started)) {
            dirty = true;
            other_input = true;
        }
        dirty |= ui
            .mail_recovery
            .advance(ui.mail_attention, now.duration_since(input_started));
        dirty |= ui.gap_notice.advance(ui.stream_gap);
        dirty |= ui.activity.tick(now);
        dirty |= ui.switcher.tick(now);
        dirty |= ui
            .transcript
            .animate_tools(now.duration_since(input_started));
        dirty |= ui
            .transcript
            .reveal_stalled(now.duration_since(input_started));
        if !ui.running() {
            typing_pause.cancel();
        }
        if !ui.quit.closing()
            && (dirty || ui.transcript.draining())
            && typing_pause.remaining(now).is_zero()
            && pacing::due(now.duration_since(last), other_input)
        {
            if let Some(movement) = ui.advance_history() {
                history_request(&mut ui, &backend, movement);
            }
            ui.transcript.drain();
            if let Err(error) =
                presenter.paint(&mut terminal, &mut ui, &mut capture, &bytes, &batch, false)
            {
                terminal_lost(&mut ui, &backend, access, input_started.elapsed(), error);
                continue;
            }
            if ui.permission.is_some() {
                ui.permission_input.presented(input_started.elapsed());
            }
            let requested_history = if !ui.views.loading() {
                if let Some(movement) = ui.navigation.boundary(&mut ui.transcript) {
                    history_request(&mut ui, &backend, movement);
                    true
                } else {
                    false
                }
            } else {
                false
            };
            ui.launch.painted();
            // A launch that became ready in this batch gets another input pass.
            if ui.launch.waiting_to_send() {
                let _ = wake.try_send(());
            }
            last = now;
            typing_pause.painted(now);
            dirty = requested_history;
        }
        if ui.quit.closing() {
            let _ = notification.recv_timeout(std::time::Duration::from_millis(100));
        } else if dirty || ui.transcript.draining() {
            let remaining = FRAME_INTERVAL
                .saturating_sub(last.elapsed())
                .max(typing_pause.remaining(tokio::time::Instant::now()));
            let _ = notification.recv_timeout(remaining);
        } else if refresh_connections
            || ui.running()
            || ui.switcher.ticking()
            || mouse::ticking(&ui)
            || ui.transcript.tools_running()
            || ui.transcript.compacting()
            || ui.quit.hint().is_some()
            || ui.clipboard.hint().is_some()
        {
            let _ = notification.recv_timeout(std::time::Duration::from_millis(100));
        } else if let Some(age) = backend.updates.pass_age() {
            // A long backend pass turns the heartbeat to lagged on time, and
            // a lagged one is re-read until the pass ends.
            let until = backend::LAG_AFTER.saturating_sub(age);
            let _ = notification.recv_timeout(until.max(std::time::Duration::from_millis(100)));
        } else if ui.heartbeat.starts_with("lagged") {
            let _ = notification.recv_timeout(std::time::Duration::from_millis(100));
        } else if let Some(remaining) = ui.mail_recovery.wait(now.duration_since(input_started)) {
            let _ = notification.recv_timeout(remaining);
        } else {
            // The wake channel closing means nothing can drive this loop again.
            // That is a close like any other, so it asks for the stop instead of
            // returning past it.
            if notification.recv().is_err() {
                terminal_lost(
                    &mut ui,
                    &backend,
                    access,
                    input_started.elapsed(),
                    "the wake channel closed",
                );
            }
        }
    }
}

#[cfg(test)]
mod form_tests;
