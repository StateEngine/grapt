use crate::backend::Agent;
use crate::editor::Editor as TextArea;
use grapt_client::PermissionAsk;
use grapt_tui_transcript::{markdown::literal, palette::Palette, transcript::Transcript};
use ratatui::{
    layout::Rect,
    widgets::{Block, Paragraph},
    Frame,
};

#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub struct Ui {
    pub bindings: crate::keybindings::Bindings,
    pub chords: crate::keybindings::Matcher,
    pub command_palette: Option<crate::command_palette::Picker>,
    pub settings: Option<crate::settings::Picker>,
    pub preferences: crate::settings::Preferences,
    pub connect: crate::connect::Connect,
    pub images: crate::images::Images,
    pub views: crate::views::Views,
    pub queued: grapt_client::wire::QueuedInputPreview,
    pub followups: crate::followups::Queue,
    pub sending: crate::sending::Sending,
    pub quit: crate::quit::Quit,
    pub selection: crate::selection::Selection,
    pub clipboard: crate::clipboard::State,
    pub activity: crate::activity::Activity,
    pub transcript: Transcript,
    pub slash: crate::slash::Completion,
    pub recall: crate::recall::Recall,
    pub forms: grapt_tui_forms::Board,
    pub launch: crate::launch::Launch,
    pub composer: TextArea<'static>,
    pub mentions: crate::mentions::Mentions,
    pub agents: Vec<Agent>,
    /// Whether the last session-list read could list anything.
    pub catalog: grapt_client::CatalogReach,
    pub session_pins: crate::sessions::Pins,
    pub switcher: crate::subagents::Switcher,
    pub permission: Option<PermissionAsk>,
    pub permission_input: grapt_client::permission_input::PermissionInput,
    pub policy: Option<String>,
    pub bypass_permissions: bool,
    pub mail_recovery: crate::mail_status::Recovery,
    pub mail_attention: Option<grapt_api::u3a::MailAttentionInfo>,
    /// StateEngine continuity's level; `None` is unknown and shows nothing.
    pub continuity_attention: Option<grapt_api::u3a::ContinuityAttentionInfo>,
    /// One neutral, self-clearing line for a live-event gap.
    pub gap_notice: crate::gap_notice::Notice,
    /// The gap fact from the last backend update, so the tick loop can age
    /// the notice without waiting for another update.
    pub stream_gap: bool,
    /// The connection from the last backend update, for the heartbeat.
    pub link: crate::backend::Link,
    /// The footer's heartbeat, re-read every UI pass from the backend's own
    /// liveness and the connection; empty of any session activity.
    pub heartbeat: String,
    pub status: String,
    pub directory: Option<String>,
    pub repository_status: Option<String>,
    pub command_notice: Option<String>,
    pub feedback: Option<grapt_client::projection::feedback::Feedback>,
    pub failed_execution: Option<grapt_client::wire::ExecutionWireId>,
    failed_draft: crate::recovery::FailedDraft,
    pub usage: grapt_client::view::status::Snapshot,
    pub usage_pane: Option<crate::usage::Pane>,
    pub command_pane: Option<crate::command_pane::Pane>,
    mcp_verbose: Option<bool>,
    deferred_safety: Option<(String, Vec<String>)>,
    pub grants_pane: Option<crate::grants::Pane>,
    pub title: String,
    pub palette: Palette,
    pub picker: Option<crate::sessions::Picker>,
    pub agent_picker: Option<crate::agents::Picker>,
    pub model: Option<grapt_client::wire::ModelRow>,
    pub models: Option<crate::models::Picker>,
    pub pending: Option<String>,
    pub prompt_ready: bool,
    live_running: Option<bool>,
    /// The selected Session's running execution as last shown; a stop key
    /// aims at it so a late stop cannot reach the execution after it.
    pub live_execution: Option<grapt_client::wire::ExecutionWireId>,
    pub permission_scroll: usize,
    pub permission_details: bool,
    permission_status: String,
    pub permission_page: usize,
    pub navigation: crate::navigation::Navigation,
    pub browsing: bool,
    pub composer_area: Rect,
    pub transcript_area: Rect,
    footer_area: Rect,
    pub(crate) scrollbar_area: Rect,
    pub(crate) scrollbar_grab: Option<crate::mouse::Grab>,
}

impl Ui {
    /// Show an update's rows as the page moves it carries allow, and settle
    /// the outstanding page request after the frame loop's other updates via
    /// [`Self::settle_navigation`]. Returns whether the screen changed and
    /// whether the rows were obsolete, so their patches are dropped too.
    pub fn transcript_update(
        &mut self,
        (reset, start, rows, reconcile): (bool, usize, Vec<grapt_client::Slice>, bool),
        moves: &[crate::navigation::Move],
    ) -> (bool, bool) {
        use crate::navigation::Page;
        let page = self.navigation.page(moves);
        if page == Page::Obsolete {
            return (false, true);
        }
        // Rows another command paged in keep the reading position.
        let changed = if reconcile
            || (reset && (page == Page::Foreign || self.navigation.preserves_reading()))
        {
            self.transcript.reconcile(rows)
        } else {
            self.transcript.update(reset, start, rows)
        };
        (changed, false)
    }

    /// Settle the outstanding page request, matched by the command that sent
    /// it. A completed move places the view; one a quit discarded leaves the
    /// scroll and its anchor as they are.
    pub fn settle_navigation(
        &mut self,
        moves: &[crate::navigation::Move],
        dropped: &[crate::backend::CommandId],
    ) -> bool {
        for movement in moves {
            self.navigation.complete(*movement, &mut self.transcript);
        }
        self.navigation.dropped(dropped);
        !moves.is_empty() || !dropped.is_empty()
    }

    pub fn new(light: bool) -> Self {
        let mut composer = TextArea::default();
        composer.set_max_histories(128);
        // The editor retains logical coordinates; our presenter owns wrapping.
        composer.set_wrap_mode(ratatui_textarea::WrapMode::None);
        Self {
            bindings: Default::default(),
            chords: Default::default(),
            command_palette: None,
            settings: None,
            preferences: Default::default(),
            connect: Default::default(),
            images: Default::default(),
            views: Default::default(),
            queued: grapt_client::wire::QueuedInputPreview::default(),
            followups: Default::default(),
            sending: Default::default(),
            quit: crate::quit::Quit::default(),
            selection: crate::selection::Selection::default(),
            clipboard: crate::clipboard::State::default(),
            activity: crate::activity::Activity::default(),
            transcript: Transcript::new(),
            slash: crate::slash::Completion::default(),
            forms: grapt_tui_forms::Board::default(),
            launch: crate::launch::Launch::default(),
            composer,
            mentions: crate::mentions::Mentions::default(),
            recall: crate::recall::Recall::default(),
            agents: Vec::new(),
            catalog: grapt_client::CatalogReach::Read,
            session_pins: crate::sessions::Pins::default(),
            switcher: crate::subagents::Switcher::default(),
            permission: None,
            permission_input: Default::default(),
            policy: None,
            bypass_permissions: false,
            mail_attention: None,
            continuity_attention: None,
            mail_recovery: Default::default(),
            gap_notice: Default::default(),
            stream_gap: false,
            link: crate::backend::Link::default(),
            heartbeat: String::new(),
            status: "Connecting…".into(),
            directory: None,
            repository_status: None,
            command_notice: None,
            feedback: None,
            failed_execution: None,
            failed_draft: crate::recovery::FailedDraft::default(),
            usage: grapt_client::view::status::Snapshot::default(),
            usage_pane: None,
            command_pane: None,
            mcp_verbose: None,
            deferred_safety: None,
            grants_pane: None,
            title: "Grapt".into(),
            palette: Palette::new(light),
            picker: None,
            agent_picker: None,
            model: None,
            models: None,
            pending: None,
            prompt_ready: false,
            live_running: None,
            live_execution: None,
            permission_scroll: 0,
            permission_details: false,
            permission_status: "Connecting…".into(),
            permission_page: 1,
            navigation: crate::navigation::Navigation::default(),
            browsing: false,
            composer_area: Rect::default(),
            transcript_area: Rect::default(),
            footer_area: Rect::default(),
            scrollbar_area: Rect::default(),
            scrollbar_grab: None,
        }
    }

    pub fn mentions_enabled(&self) -> bool {
        !self.connect.visible()
            && self.command_palette.is_none()
            && self.settings.is_none()
            && self.permission.is_none()
            && self.picker.is_none()
            && self.agent_picker.is_none()
            && self.models.is_none()
            && self.usage_pane.is_none()
            && self.command_pane.is_none()
            && self.grants_pane.is_none()
            && !self.forms.focused
            && !self.recall.searching()
            && !self.launch.has_intent()
            && self.pending.is_none()
    }

    pub fn permission_update(&mut self, permission: Option<PermissionAsk>) -> bool {
        if self
            .permission
            .as_ref()
            .map(|ask| (&ask.session_id, &ask.request_id))
            != permission
                .as_ref()
                .map(|ask| (&ask.session_id, &ask.request_id))
        {
            self.permission_input = Default::default();
            self.permission_scroll = 0;
            self.permission_details = false;
            self.permission_status.clone_from(&self.status);
        }
        let changed = self.permission != permission;
        self.permission = permission;
        changed
    }

    pub fn question_band_height(&self, height: u16) -> Option<u16> {
        (self.permission.is_none() && self.forms.input_band())
            .then_some(height.saturating_sub(8).min(18))
    }

    pub fn model_update(
        &mut self,
        model: Option<grapt_client::wire::ModelRow>,
        rows: Option<Vec<grapt_client::wire::ModelRow>>,
        result: Option<bool>,
    ) -> bool {
        let changed = self.model != model || rows.is_some() || result.is_some();
        self.model = model;
        if rows.is_some() && self.agent_picker.is_some() {
            self.agent_picker = None;
            self.models = Some(crate::models::Picker::default());
        }
        if let Some(picker) = &mut self.models {
            if let Some(rows) = rows {
                self.agent_picker = None;
                picker.loaded(rows, self.model.as_ref());
            }
            if let Some(accepted) = result {
                if accepted {
                    self.models = None;
                } else {
                    picker.pending = false;
                }
            }
        }
        changed
    }

    /// Selection fencing must discard stale state, not a prior effect warning.
    pub fn accept_selection_update(
        &mut self,
        ticket: Option<crate::backend::CommandId>,
        commands: &mut [Option<crate::backend::commands::Completion>],
    ) -> bool {
        let current = self.views.accept(ticket);
        for command in commands.iter_mut().filter(|_| !current) {
            if let Some(done) = command.take_if(|done| done.safety_only()) {
                self.defer_safety(done);
            }
        }
        current
    }

    /// A result for a session no longer shown keeps only a safety notice.
    pub fn defer_safety(&mut self, done: crate::backend::commands::Completion) -> bool {
        if !done.safety_only() {
            return false;
        }
        if let crate::backend::commands::Completion {
            view: Some(crate::backend::commands::View::NoticeText { notice, rows }),
            ..
        } = done
        {
            // At most one bounded safety result, never a queue of commands.
            self.deferred_safety = Some((notice, rows));
        }
        false
    }

    /// Called after the accepted update's session state has been installed.
    pub fn publish_command_result(
        &mut self,
        command: Option<crate::backend::commands::Completion>,
    ) -> bool {
        let mut changed = false;
        if let Some(command) = command {
            self.command_update(command);
            changed = true;
        }
        if let Some((notice, rows)) = self.deferred_safety.take() {
            // Do not settle or replace the new view's pending human draft.
            self.show_notice_text(notice, rows);
            changed = true;
        }
        changed
    }

    fn show_notice_text(&mut self, notice: String, rows: Vec<String>) {
        self.mcp_verbose = None;
        self.usage_pane = None;
        self.agent_picker = None;
        self.picker = None;
        self.models = None;
        self.command_notice = Some(notice);
        self.command_pane = Some(crate::command_pane::Pane::new(rows));
    }

    pub fn command_update(&mut self, done: crate::backend::commands::Completion) {
        use crate::backend::commands::View;
        self.pending = None;
        if done.consume || done.replacement.is_some() {
            // The user may have edited while the protocol worker was busy.
            // Reuse submission's exact-draft check before clearing or expanding.
            self.recall.replace_if_unchanged(
                &done.text,
                done.replacement.as_deref().unwrap_or_default(),
                &mut self.composer,
            );
        }

        if done.view.is_some() {
            self.mcp_verbose = None;
            self.usage_pane = None;
            self.command_pane = None;
            self.grants_pane = None;
            self.agent_picker = None;
            self.picker = None;
            self.models = None;
        }
        match done.view {
            Some(View::Grants(snapshot)) => {
                self.grants_pane = Some(crate::grants::Pane::new(snapshot))
            }
            Some(View::Status) => self.usage_pane = Some(crate::usage::Pane::default()),
            Some(View::Models) => self.models = Some(crate::models::Picker::default()),
            Some(View::Sessions) => self.picker = Some(crate::sessions::Picker::new(&self.agents)),
            Some(View::Notice(message)) => self.command_notice = Some(message),
            Some(View::NoticeText { notice, rows }) => {
                self.show_notice_text(notice, rows);
            }
            Some(View::Mcp { rows, verbose }) => {
                self.mcp_verbose = Some(verbose);
                self.command_pane = Some(crate::command_pane::Pane::new(rows));
            }
            Some(View::Text(rows)) => {
                self.command_pane = Some(crate::command_pane::Pane::new(rows))
            }
            None => {}
        }
    }

    pub fn mcp_update(&mut self, summary: Vec<String>, detail: Vec<String>) -> bool {
        let (Some(verbose), Some(pane)) = (self.mcp_verbose, &mut self.command_pane) else {
            return false;
        };
        pane.replace(if verbose { detail } else { summary })
    }

    pub fn workspace_update(&mut self, directory: Option<&str>) -> bool {
        if self.directory.as_deref() == directory {
            return false;
        }
        self.directory = directory.map(str::to_owned);
        true
    }

    pub fn session_update(
        &mut self,
        selected: Option<grapt_client::wire::SessionWireId>,
        running: bool,
    ) -> bool {
        let initialized = self.live_running.is_some();
        let activity_changed = self.live_running != Some(running);
        self.live_running = Some(running);
        let changed = self.recall.session(selected, &mut self.composer);
        if changed {
            self.chords.cancel();
            self.mail_recovery = crate::mail_status::Recovery::default();
            self.activity = crate::activity::Activity::default();
            self.failed_draft = crate::recovery::FailedDraft::default();
            crate::mouse::cancel(self);
            self.selection.clear();
            self.clipboard.reset();
            self.agent_picker = None;
            self.navigation.reset();
            self.command_notice = None;
            self.command_pane = None;
            self.grants_pane = None;
            if initialized {
                self.models = None;
            }
        }
        changed || activity_changed
    }

    pub fn submission_accepted(&mut self, text: &str) -> bool {
        self.failed_draft.submitted(text);
        self.recall.submitted(text, &mut self.composer)
    }

    pub fn restore_failed_input(&mut self) -> bool {
        if !matches!(
            self.feedback,
            Some(grapt_client::projection::feedback::Feedback::Failed { .. })
        ) || self.pending.is_some()
            || self.running()
            || self.browsing
            || self.recall.searching()
            || self.launch.has_intent()
        {
            return false;
        }
        self.failed_draft.restore(
            self.failed_execution.as_ref(),
            self.transcript.latest_prompt(),
            &mut self.composer,
        )
    }

    pub fn policy_banner(&self) -> Option<&str> {
        self.policy.as_deref().and_then(|line| {
            let line = line
                .strip_prefix("Bypass permissions on | ")
                .or_else(|| line.strip_prefix("Bypass permissions on (last confirmed) | "))
                .unwrap_or(line);
            (!line.starts_with("Preset:")).then_some(line)
        })
    }

    pub fn running(&self) -> bool {
        // Before the first backend snapshot, retain the existing session-row
        // presentation. Afterwards, the selected session may be outside that page.
        self.live_running.unwrap_or_else(|| {
            self.agents
                .iter()
                .any(|agent| agent.current && crate::subagents::running(&agent.status))
        })
    }

    /// What the running turn is waiting on, from facts already installed.
    ///
    /// A provider-slot wait outranks compaction because the slot decision is
    /// what gates progress; both are reported while the turn is still running.
    pub fn activity_phase(&self) -> crate::activity::Phase {
        if matches!(
            self.feedback,
            Some(grapt_client::projection::feedback::Feedback::WaitingForCapacity { .. })
        ) {
            return crate::activity::Phase::WaitingForCapacity;
        }
        if matches!(
            self.feedback,
            Some(grapt_client::projection::feedback::Feedback::WaitingForProviderSlot { .. })
        ) {
            return crate::activity::Phase::WaitingForProvider;
        }
        if self.transcript.compacting() {
            return crate::activity::Phase::Compacting;
        }
        crate::activity::Phase::Thinking
    }

    /// Whether the seat is actively executing (not paused on user input or
    /// startup, and not settled in a failed state).
    pub fn display_running(&self, running: bool) -> bool {
        running
            && self.failed_execution.is_none()
            && self.permission.is_none()
            && !self.forms.input_band()
            && self.launch.readiness != crate::launch::Readiness::Paused
    }

    /// Install the update's form state before observing activity, so clearing
    /// an assistant question restores `Thinking` on the first resumed update.
    pub fn apply_forms_and_activity(
        &mut self,
        selected: Option<grapt_client::wire::SessionWireId>,
        form_location: Option<grapt_client::wire::FormLocation>,
        forms: Vec<grapt_client::wire::FormRow>,
        running: bool,
        streamed_tokens: Option<usize>,
        now: tokio::time::Instant,
    ) -> bool {
        let mut dirty = self.forms.update(selected, form_location, forms);
        dirty |= self.activity.observe(
            self.display_running(running),
            streamed_tokens,
            self.activity_phase(),
            now,
        );
        dirty
    }

    pub fn activity_label(&self) -> Option<String> {
        if !self.display_running(true) {
            return None;
        }
        let mut label = self.activity.label()?;
        // Name the running tool and how long THAT tool has run, so the row says
        // what the seat is doing and the number beside it is not the turn's.
        //
        // Only newlines are neutralised: a tool title commonly carries a file
        // name, and rewriting its underscores would rename what the user sees.
        if let Some((tool, elapsed)) = self.transcript.running_tool_elapsed() {
            label.push_str(" · ");
            label.push_str(&literal(tool).replace('\n', " "));
            label.push_str(&format!(" ({})", short_elapsed(elapsed)));
        }
        if let Some(agent) = self
            .agents
            .iter()
            .find(|a| a.current && a.row.parent.is_some())
        {
            label.push_str(" · ");
            label.push_str(&literal(&agent.row.title).replace(['\n', '_'], " "));
        }
        Some(label)
    }

    pub fn draw_stream(
        &mut self,
        frame: &mut Frame<'_>,
        previous: Option<&ratatui::buffer::Buffer>,
    ) {
        if !self
            .selection
            .hold(frame.buffer_mut(), self.transcript_area)
        {
            self.transcript
                .paint_changed(frame, self.transcript_area, self.palette, previous);
        }
        self.transcript
            .draw_scrollbar(frame, self.scrollbar_area, self.palette);
        crate::composer::status(
            frame,
            self.composer_area,
            self.activity_label().as_deref(),
            self.palette,
        );
        self.draw_footer(frame, self.footer_area);
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let p = self.palette;
        frame.render_widget(Block::default().style(p.normal()), area);
        if self.connect.visible()
            && self.permission.is_none()
            && !self.agents.iter().any(|agent| agent.current)
        {
            crate::chrome::header(
                frame,
                Rect::new(area.x, area.y, area.width, 1),
                &self.title,
                p,
            );
            self.connect.draw(frame, center(area, 90, 85), p);
            crate::chrome::draw_bypass(
                self,
                frame,
                Rect {
                    y: area
                        .bottom()
                        .saturating_sub(crate::chrome::footer_height(self, area.width)),
                    height: crate::chrome::footer_height(self, area.width).min(area.height),
                    ..area
                },
            );
            return;
        }
        let (composer_rows, cursor_row) =
            grapt_tui_forms::editor::rows(&self.composer, area.width.saturating_sub(4), p);
        let recovery = crate::recovery::rows(self, usize::from(crate::chrome::inner(area).width));
        let policy = self.policy_banner();
        let policy_rows = usize::from(policy.is_some());
        let switcher_rows = self.switcher.height(&self.agents, area.height);
        let vertical = crate::composer::layout(
            area,
            composer_rows.len() + self.images.len(),
            recovery.len() + policy_rows,
            switcher_rows,
            self.activity.active(),
            self.question_band_height(area.height),
            crate::chrome::footer_height(self, area.width),
        );
        if let Some(policy) = policy {
            frame.render_widget(
                Paragraph::new(grapt_client::cells::cut(
                    &literal(policy).replace('\n', " "),
                    usize::from(crate::chrome::inner(area).width),
                ))
                .style(p.normal().fg(if policy.starts_with("BYPASS") {
                    p.bad
                } else {
                    p.accent
                })),
                Rect {
                    y: area.y.saturating_add(1),
                    height: vertical[0].height.saturating_sub(1).min(1),
                    ..crate::chrome::inner(vertical[0])
                },
            );
        }
        frame.render_widget(
            Paragraph::new(recovery.join("\n")).style(
                p.normal()
                    .fg(
                        if self.command_notice.is_some()
                            && self.feedback.is_none()
                            && matches!(self.status.as_str(), "Connected" | "Connecting…")
                        {
                            p.accent
                        } else {
                            p.bad
                        },
                    )
                    .bg(p.panel),
            ),
            Rect {
                y: area.y.saturating_add(1 + policy_rows as u16),
                height: vertical[0].height.saturating_sub(1 + policy_rows as u16),
                ..crate::chrome::inner(vertical[0])
            },
        );
        crate::chrome::header(frame, vertical[0], &self.title, p);
        let mut conversation = vertical[1];
        let permission_area = self.permission.as_ref().map(|ask| {
            let width = conversation.width.saturating_sub(4);
            let height = crate::permission::height(
                ask,
                (self.status != "Connected").then_some(self.status.as_str()),
                width,
                self.permission_details,
            )
            .min(conversation.height.saturating_sub(1));
            conversation.height = conversation.height.saturating_sub(height);
            Rect::new(
                conversation.x.saturating_add(2),
                conversation.bottom(),
                width,
                height,
            )
        });
        let mut queued = grapt_client::view::queued_input_lines(
            &self.queued,
            usize::from(conversation.width.saturating_sub(4)),
        );
        queued.extend(
            self.sending
                .lines(usize::from(conversation.width.saturating_sub(4))),
        );
        queued.extend(
            self.followups
                .lines(usize::from(conversation.width.saturating_sub(4))),
        );
        let queued_height = (queued.len() as u16).min(conversation.height.saturating_sub(1));
        let transcript = Rect {
            x: conversation.x.saturating_add(2),
            width: conversation.width.saturating_sub(4),
            height: conversation.height.saturating_sub(queued_height),
            ..conversation
        };
        self.transcript_area = transcript;
        if !self.selection.hold(frame.buffer_mut(), transcript) {
            self.transcript.paint(frame, transcript, p);
        }
        frame.render_widget(
            Paragraph::new(queued.join("\n")).style(p.normal().fg(p.muted)),
            Rect {
                y: conversation.bottom().saturating_sub(queued_height),
                height: queued_height,
                ..transcript
            },
        );
        self.scrollbar_area = Rect::new(
            conversation.right().saturating_sub(1),
            conversation.y,
            u16::from(conversation.width > 0),
            transcript.height,
        );
        self.transcript
            .draw_scrollbar(frame, self.scrollbar_area, p);
        self.composer_area = vertical[2];
        crate::composer::paint(
            frame,
            vertical[2],
            composer_rows,
            cursor_row,
            p,
            self.activity_label().as_deref(),
            &mut self.images,
        );
        if self.mentions_enabled() && !self.forms.input_band() {
            self.mentions.draw(frame, vertical[2], p);
        }
        self.switcher
            .draw(frame.buffer_mut(), vertical[3], &self.agents, p);
        self.footer_area = vertical[4];
        self.draw_footer(frame, vertical[4]);
        if let Some(ask) = &self.permission {
            if self.permission_status != self.status {
                if self.status != "Connected" {
                    self.permission_scroll = 0;
                }
                self.permission_status.clone_from(&self.status);
            }
            let notice = (self.status != "Connected").then_some(self.status.as_str());
            if let Some(panel) = permission_area {
                self.permission_page = crate::permission::draw(
                    frame,
                    panel,
                    ask,
                    notice,
                    &mut self.permission_scroll,
                    self.permission_details,
                    p,
                );
            }
            return;
        }
        if let Some(pane) = &mut self.usage_pane {
            pane.draw(
                frame,
                center(area, 94, 90),
                &self.usage,
                self.policy.as_deref(),
                p,
            );
        }
        if let Some(pane) = &mut self.grants_pane {
            pane.draw(frame, center(area, 94, 90), p);
        }
        if let Some(pane) = &mut self.command_pane {
            pane.draw(frame, center(area, 94, 90), p);
        }
        if let Some(picker) = &mut self.picker {
            picker.draw(
                frame,
                center(area, 80, 80),
                &self.agents,
                &self.session_pins,
                self.catalog,
                p,
            );
        }
        if let Some(picker) = &mut self.agent_picker {
            picker.draw(frame, center(area, 80, 80), p);
        }
        if let Some(picker) = &mut self.models {
            picker.draw(frame, center(area, 90, 80), self.model.as_ref(), p);
        }
        if self.forms.focused
            && self.command_pane.is_none()
            && self.grants_pane.is_none()
            && self.picker.is_none()
            && self.models.is_none()
            && self.usage_pane.is_none()
        {
            if self.forms.input_band() {
                self.forms.draw_ask(frame, vertical[2], p);
            } else {
                self.forms.draw(frame, center(area, 94, 90), p);
            }
        }
        if !self.forms.input_band() {
            self.slash.draw(frame, self.composer_area, p);
        }
        crate::command_palette::draw(self, frame, center(area, 94, 85));
        if let Some(picker) = &self.settings {
            let mut values = self.preferences.values;
            values.light = self.palette.light;
            picker.draw(frame, center(area, 80, 65), values, p);
        }
        self.connect.draw(frame, center(area, 90, 85), p);
        crate::chrome::draw_bypass(self, frame, self.footer_area);
    }

    fn draw_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        crate::chrome::draw_footer(self, frame, area);
    }
}

/// A short, bounded duration for a single row: seconds, then minutes, then
/// hours. Tool waits are usually seconds, and the longer forms stay short
/// enough for the footer at narrow widths.
fn short_elapsed(elapsed: std::time::Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        n if n >= 3600 => format!("{}h {}m", n / 3600, n / 60 % 60),
        n if n >= 60 => format!("{}m {}s", n / 60, n % 60),
        n => format!("{n}s"),
    }
}

fn center(area: Rect, width: u16, height: u16) -> Rect {
    let w = area.width.saturating_mul(width) / 100;
    let h = area.height.saturating_mul(height) / 100;
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::CommandId;
    use grapt_client::client::HistoryMove;

    /// One long entry, laid out and read at bottom offset 37.
    fn history() -> Vec<grapt_client::Slice> {
        vec![grapt_client::Slice {
            compaction: None,
            kind: grapt_client::projection::SliceKind::User,
            tool_id: None,
            tool_phase: None,
            tool_summary: None,
            ordinal: 0,
            text: (0..120).map(|i| format!("PAGE_{i:03}\n")).collect(),
            read_detail_start: None,
            marks: Vec::new(),
            closed: true,
            footer: None,
            truncated_bytes: 0,
        }]
    }

    /// A UI at bottom offset 37 and a source of command ids.
    fn reading_at_37() -> (Ui, crate::backend::Handoff) {
        let mut ui = Ui::new(false);
        ui.transcript.update(true, 0, history());
        ui.transcript.scroll = 37;
        ui.transcript.visible(40, 10, Palette::new(false));
        (ui, crate::backend::Handoff::default())
    }

    fn sent(ui: &mut Ui, handoff: &crate::backend::Handoff, movement: HistoryMove) -> CommandId {
        ui.navigation.jump(movement);
        let id = handoff
            .envelope(crate::backend::Command::Browse(movement))
            .0;
        ui.navigation.sent(id);
        id
    }

    /// An update carrying a fresh page, applied as the frame loop applies it.
    fn paged(ui: &mut Ui, moves: &[crate::navigation::Move], dropped: &[CommandId]) {
        ui.transcript_update((true, 0, history(), false), moves);
        ui.settle_navigation(moves, dropped);
    }

    #[test]
    fn a_page_request_a_quit_discarded_leaves_the_view_where_it_was() {
        let (mut ui, handoff) = reading_at_37();
        let home = sent(&mut ui, &handoff, HistoryMove::Beginning);
        assert!(ui.settle_navigation(&[], &[home]));
        assert!(!ui.navigation.loading());
        assert_eq!((ui.transcript.from_top, ui.transcript.scroll), (false, 37));
    }

    #[test]
    fn an_older_same_direction_move_never_settles_the_discarded_one() {
        let (mut ui, handoff) = reading_at_37();
        // Ctrl+Home ran; a second Ctrl+Home was discarded by a refused quit.
        let first = sent(&mut ui, &handoff, HistoryMove::Beginning);
        let second = sent(&mut ui, &handoff, HistoryMove::Beginning);
        paged(&mut ui, &[(first, HistoryMove::Beginning, true)], &[second]);
        assert!(!ui.navigation.loading());
        assert_eq!((ui.transcript.from_top, ui.transcript.scroll), (false, 37));
    }

    #[test]
    fn a_late_discard_never_clears_a_fresh_request() {
        let (mut ui, handoff) = reading_at_37();
        let discarded = sent(&mut ui, &handoff, HistoryMove::Beginning);
        // After the refused quit, Ctrl+End asks again before the discard lands.
        let fresh = sent(&mut ui, &handoff, HistoryMove::Latest);
        assert!(ui.settle_navigation(&[], &[discarded]));
        assert!(ui.navigation.loading(), "the fresh request was cleared");
        paged(&mut ui, &[(fresh, HistoryMove::Latest, true)], &[]);
        assert!(!ui.navigation.loading());
    }

    use crate::activity::Phase;
    use grapt_client::projection::feedback::Feedback;

    /// A running tool slice the transcript can present, without a live session.
    fn running_tool(title: &str) -> Result<grapt_client::Slice, Box<dyn std::error::Error>> {
        Ok(grapt_client::Slice {
            compaction: None,
            footer: None,
            kind: grapt_client::projection::SliceKind::Tool,
            tool_id: None,
            tool_phase: Some(grapt_client::projection::ToolPhase::Running),
            tool_summary: Some(grapt_client::projection::ToolSummary {
                terminal_session: None,
                stdin: None,
                title: title.into(),
                duration_ms: None,
                preview: Vec::new(),
                omitted_lines: 0,
                error: None,
                name: "shell".into(),
                argument: "se --help".into(),
                result: String::new(),
            }),
            ordinal: 0,
            text: "tool shell\n".into(),
            read_detail_start: None,
            marks: Vec::new(),
            closed: false,
            truncated_bytes: 0,
        })
    }

    #[test]
    fn each_phase_has_its_own_label_and_provider_slot_outranks_compaction() {
        let mut ui = Ui::new(false);
        ui.status = "Connected".into();
        assert_eq!(ui.activity_phase(), Phase::Thinking);

        ui.transcript.observe_compacting(true);
        assert_eq!(ui.activity_phase(), Phase::Compacting);

        // A queued provider slot gates progress even while a compaction is
        // recorded, so it wins the label.
        ui.feedback = Some(Feedback::WaitingForProviderSlot {
            message: "run breathing".into(),
        });
        assert_eq!(ui.activity_phase(), Phase::WaitingForProvider);

        ui.feedback = None;
        ui.transcript.observe_compacting(false);
        assert_eq!(ui.activity_phase(), Phase::Thinking);
    }

    #[test]
    fn activity_label_names_the_running_tool_and_drops_it_when_the_tool_completes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let now = tokio::time::Instant::now();
        let mut ui = Ui::new(false);
        ui.activity.observe(true, None, Phase::Thinking, now);
        ui.transcript.observe_running(true);
        ui.transcript
            .update(true, 0, vec![running_tool("Run bounded command")?]);
        let running = ui.activity_label().ok_or("running label")?;
        assert!(
            running.contains("Run bounded command"),
            "the row names the work: {running}"
        );

        let mut done = running_tool("Run bounded command")?;
        done.tool_phase = Some(grapt_client::projection::ToolPhase::Completed);
        done.closed = true;
        ui.transcript.update(false, 0, vec![done]);
        let finished = ui.activity_label().ok_or("finished label")?;
        assert!(
            !finished.contains("Run bounded command"),
            "a completed tool must leave the row: {finished}"
        );
        Ok(())
    }

    #[test]
    fn paused_and_failed_sessions_never_display_thinking() -> Result<(), Box<dyn std::error::Error>>
    {
        let now = tokio::time::Instant::now();
        let mut ui = Ui::new(false);
        ui.activity.observe(true, None, Phase::Thinking, now);
        assert!(ui.activity_label().is_some());

        ui.launch.readiness = crate::launch::Readiness::Paused;
        assert!(!ui.display_running(true));
        assert!(
            ui.activity_label().is_none(),
            "paused readiness must not display Thinking"
        );

        ui.launch.readiness = crate::launch::Readiness::Ready;
        let failed: grapt_client::wire::ExecutionWireId = serde_json::from_str("\"exec-failed\"")?;
        ui.failed_execution = Some(failed);
        assert!(!ui.display_running(true));
        assert!(
            ui.activity_label().is_none(),
            "failed execution must not display Thinking"
        );

        ui.failed_execution = None;
        assert!(ui.display_running(true));
        assert!(ui.activity_label().is_some());
        Ok(())
    }

    #[test]
    fn clearing_a_question_form_restores_thinking_on_the_first_resumed_update(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let now = tokio::time::Instant::now();
        let mut ui = Ui::new(false);
        let session = grapt_client::wire::session_id("ses_resume")?;
        let ask_form = grapt_client::wire::FormRow {
            metadata: [("kind".into(), serde_json::json!("request_user_input"))].into(),
            id: grapt_client::wire::form_id("frm_resume")?,
            owner: grapt_client::wire::FormOwner::Session(session.clone()),
            title: "Clarify".into(),
            fields: vec![grapt_client::wire::FormField {
                attributes: Default::default(),
                key: "answer".into(),
                kind: "string".into(),
                title: None,
                required: true,
            }],
        };

        // While the assistant question occupies the input band, Thinking stays hidden.
        ui.apply_forms_and_activity(Some(session.clone()), None, vec![ask_form], true, None, now);
        assert!(ui.forms.input_band());
        assert!(ui.activity_label().is_none());

        // Clearing forms through the real update helper restores Thinking on the
        // very first resumed update.
        ui.apply_forms_and_activity(Some(session), None, Vec::new(), true, None, now);
        assert!(!ui.forms.input_band());
        assert!(
            ui.activity_label().is_some(),
            "first resumed update after clearing question form must display Thinking"
        );
        Ok(())
    }
}
