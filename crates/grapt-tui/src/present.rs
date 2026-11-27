//! Retain the last complete canvas so typing does not revisit conversation layout.
use crate::{composer, output, ui::Ui};
use crossterm::{
    execute,
    terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate},
};
use grapt_tui_transcript::profile;
use ratatui::{
    backend::Backend, buffer::Buffer, layout::Rect, Frame, Terminal, TerminalOptions, Viewport,
};
use std::{cell::Cell, io::Write};

/// Resize events own geometry after attachment; a paint needs no console RPC.
pub fn terminal<B: Backend>(backend: B) -> Result<Terminal<B>, B::Error> {
    let area = backend.size()?.into();
    let mut terminal = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(area),
        },
    )?;
    // Apache-2.0, Copyright 2025 OpenAI.
    // A fresh diff buffer cannot describe foreign text already on the screen,
    // including default-style spaces in the last row.
    terminal.backend_mut().clear()?;
    Ok(terminal)
}

#[derive(Default)]
pub struct Presenter {
    canvas: Option<Buffer>,
    composer: Rect,
    transcript_focus: bool,
    chrome: Option<Chrome>,
}

impl Presenter {
    pub fn capture_selection(&self, ui: &mut Ui) {
        if let Some(canvas) = &self.canvas {
            ui.selection.capture(canvas, ui.transcript_area);
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn render(&mut self, ui: &mut Ui, frame: &mut Frame<'_>, input: bool) {
        let enabled = ui.permission.is_none()
            && ui.picker.is_none()
            && ui.agent_picker.is_none()
            && ui.models.is_none()
            && !ui.connect.visible()
            && ui.command_palette.is_none()
            && ui.settings.is_none()
            && ui.usage_pane.is_none()
            && ui.command_pane.is_none()
            && !ui.forms.focused
            && !ui.recall.searching()
            && ui.pending.is_none()
            && ui.launch.readiness.attached();
        ui.slash.sync(
            &ui.composer.lines().join("\n"),
            ui.composer.cursor(),
            enabled,
        );
        if input
            && self.transcript_focus == ui.transcript.tools_focused()
            && self.chrome.as_ref().is_some_and(|chrome| {
                chrome.policy == ui.policy
                    && chrome.bypass_permissions == ui.bypass_permissions
                    && chrome.mail_attention == ui.mail_attention
                    && chrome.continuity_attention == ui.continuity_attention
                    && chrome.mail_recovery == ui.mail_recovery
                    && chrome.clipboard_hint.as_deref() == ui.clipboard.hint()
                    && chrome.key_chord.as_deref() == ui.chords.hint()
                    && chrome.queued == ui.queued
                    && chrome.followups == ui.followups.revision()
                    && chrome.sending == ui.sending.revision()
                    && chrome.images == ui.images.revision()
            })
            && ui.permission.is_none()
            && ui.picker.is_none()
            && ui.agent_picker.is_none()
            && ui.models.is_none()
            && !ui.connect.visible()
            && ui.command_palette.is_none()
            && ui.settings.is_none()
            && ui.usage_pane.is_none()
            && !ui.forms.focused
            && ui.command_pane.is_none()
            && !ui.recall.searching()
            && !ui.mentions.visible()
            && !ui.slash.visible()
        {
            if let Some(canvas) = &self.canvas {
                let (rows, cursor) = grapt_tui_forms::editor::rows(
                    &ui.composer,
                    frame.area().width.saturating_sub(4),
                    ui.palette,
                );
                let area = composer::area(
                    frame.area(),
                    rows.len() + ui.images.len(),
                    crate::recovery::rows(
                        ui,
                        usize::from(crate::chrome::inner(frame.area()).width),
                    )
                    .len()
                        + usize::from(ui.policy_banner().is_some()),
                    ui.switcher.height(&ui.agents, frame.area().height),
                    ui.activity.active(),
                    crate::chrome::footer_height(ui, frame.area().width),
                );
                if canvas.area == frame.area() && area == self.composer {
                    copy_canvas(frame.buffer_mut(), canvas);
                    composer::paint(
                        frame,
                        area,
                        rows,
                        cursor,
                        ui.palette,
                        ui.activity_label().as_deref(),
                        &mut ui.images,
                    );
                    return;
                }
            }
        }
        let clean = ui.permission.is_none()
            && ui.picker.is_none()
            && ui.agent_picker.is_none()
            && ui.models.is_none()
            && !ui.connect.visible()
            && ui.command_palette.is_none()
            && ui.settings.is_none()
            && ui.usage_pane.is_none()
            && !ui.forms.focused
            && ui.command_pane.is_none()
            && !ui.recall.searching()
            && !ui.mentions.visible()
            && !ui.slash.visible();
        if clean
            && self
                .chrome
                .as_ref()
                .is_some_and(|chrome| chrome.matches(ui, frame.area()))
        {
            if let Some(canvas) = &self.canvas {
                copy_canvas(frame.buffer_mut(), canvas);
            }
            ui.draw_stream(frame, self.canvas.as_ref());
        } else {
            ui.draw(frame);
            self.chrome = clean.then(|| Chrome::capture(ui, frame.area()));
        }
        self.composer = ui.composer_area;
        self.transcript_focus = ui.transcript.tools_focused();
        match &mut self.canvas {
            Some(canvas) => copy_canvas(canvas, frame.buffer_mut()),
            None => self.canvas = Some(frame.buffer_mut().clone()),
        }
    }

    pub fn paint<B: Backend + Write>(
        &mut self,
        terminal: &mut Terminal<B>,
        ui: &mut Ui,
        capture: &mut profile::Capture,
        bytes: &Cell<u64>,
        batch: &output::Batch,
        input: bool,
    ) -> Result<(), String> {
        let started = capture.begin();
        bytes.set(0);
        let frame = batch.begin();
        execute!(terminal.backend_mut(), BeginSynchronizedUpdate).map_err(|e| e.to_string())?;
        let drawn = terminal
            .draw(|frame| {
                self.render(ui, frame, input);
                ui.selection
                    .paint(frame.buffer_mut(), ui.transcript_area, ui.palette);
            })
            .map(|_| ());
        let ended = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
        drop(frame);
        let flushed = Write::flush(terminal.backend_mut());
        drawn.map_err(|e| e.to_string())?;
        ended.map_err(|e| e.to_string())?;
        flushed.map_err(|e| e.to_string())?;
        capture
            .record(started, bytes.get())
            .map_err(|e| e.to_string())
    }
}

// Only presentation metadata is retained; protocol state keeps its existing owner.
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
struct Chrome {
    area: Rect,
    footer_height: u16,
    images: u64,
    palette: grapt_tui_transcript::palette::Palette,
    agents: Vec<crate::backend::Agent>,
    model: Option<grapt_client::wire::ModelRow>,
    title: String,
    status: String,
    command_notice: Option<String>,
    clipboard_hint: Option<String>,
    key_chord: Option<String>,
    policy: Option<String>,
    bypass_permissions: bool,
    queued: grapt_client::wire::QueuedInputPreview,
    followups: u64,
    sending: u64,
    mail_attention: Option<grapt_api::u3a::MailAttentionInfo>,
    continuity_attention: Option<grapt_api::u3a::ContinuityAttentionInfo>,
    mail_recovery: crate::mail_status::Recovery,
    usage: grapt_client::view::status::Snapshot,
    feedback: Option<grapt_client::projection::feedback::Feedback>,
    composer: Vec<String>,
    cursor: ratatui_textarea::DataCursor,
    loading: bool,
    running: bool,
    activity: bool,
    browsing: bool,
    switcher: (Option<usize>, Option<usize>, usize, u64),
}
impl Chrome {
    fn capture(ui: &Ui, area: Rect) -> Self {
        Self {
            area,
            footer_height: crate::chrome::footer_height(ui, area.width),
            images: ui.images.revision(),
            palette: ui.palette,
            agents: ui.agents.clone(),
            model: ui.model.clone(),
            title: ui.title.clone(),
            status: ui.status.clone(),
            command_notice: ui.command_notice.clone(),
            clipboard_hint: ui.clipboard.hint().map(str::to_owned),
            key_chord: ui.chords.hint().map(str::to_owned),
            policy: ui.policy.clone(),
            bypass_permissions: ui.bypass_permissions,
            queued: ui.queued.clone(),
            followups: ui.followups.revision(),
            sending: ui.sending.revision(),
            mail_attention: ui.mail_attention,
            continuity_attention: ui.continuity_attention,
            mail_recovery: ui.mail_recovery.clone(),
            usage: ui.usage.clone(),
            feedback: ui.feedback.clone(),
            composer: ui.composer.lines().to_vec(),
            cursor: ui.composer.cursor(),
            loading: ui.navigation.loading(),
            running: ui.running(),
            activity: ui.activity.active(),
            browsing: ui.browsing,
            switcher: ui.switcher.view(),
        }
    }
    fn matches(&self, ui: &Ui, area: Rect) -> bool {
        self.area == area
            && self.footer_height == crate::chrome::footer_height(ui, area.width)
            && self.images == ui.images.revision()
            && self.palette == ui.palette
            && self.agents == ui.agents
            && self.model == ui.model
            && self.title == ui.title
            && self.status == ui.status
            && self.command_notice == ui.command_notice
            && self.clipboard_hint.as_deref() == ui.clipboard.hint()
            && self.key_chord.as_deref() == ui.chords.hint()
            && self.policy == ui.policy
            && self.bypass_permissions == ui.bypass_permissions
            && self.queued == ui.queued
            && self.followups == ui.followups.revision()
            && self.sending == ui.sending.revision()
            && self.mail_attention == ui.mail_attention
            && self.continuity_attention == ui.continuity_attention
            && self.mail_recovery == ui.mail_recovery
            && self.usage == ui.usage
            && self.feedback == ui.feedback
            && self.composer == ui.composer.lines()
            && self.cursor == ui.composer.cursor()
            && self.loading == ui.navigation.loading()
            && self.running == ui.running()
            && self.activity == ui.activity.active()
            && self.browsing == ui.browsing
            && self.switcher == ui.switcher.view()
    }
}

#[cfg(test)]
mod canvas_tests;
#[cfg(all(test, not(windows)))]
mod flush_tests;
#[cfg(test)]
mod geometry_tests;
#[cfg(test)]
mod tests;

// Buffer derives Clone, whose default clone_from replaces its complete Vec.
// Reuse cell storage after the terminal dimensions have settled.
fn copy_canvas(target: &mut Buffer, source: &Buffer) {
    target.area = source.area;
    target.content.clone_from(&source.content);
}
