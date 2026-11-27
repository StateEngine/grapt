//! Notify the user when R needs them: a turn that completed, or an approval
//! or question now waiting. Completion alerts require an unfocused terminal
//! or idle user; waiting requests always alert. No notification timer.
//!
//! Copyright 2025 OpenAI. Modified by Grapt: an idle-aware emission condition, a
//! bounded environment probe instead of the terminal-detection crate, and
//! fixed short lines instead of model-content previews.

use crossterm::event::{Event, KeyEventKind};
use grapt_client::{
    wire::{FormOwner, FormRow, FormWireId, PermissionWireId, SessionWireId},
    PermissionAsk,
};
use std::io::Write;
use std::time::Duration;

/// No key or paste for this long counts as an idle user.
pub const IDLE_AFTER: Duration = Duration::from_secs(5);

/// The one short line for each waiting state.
pub const TURN_COMPLETE: &str = "Grapt: turn complete";
pub const APPROVAL_WAITING: &str = "Grapt: approval needed";
pub const QUESTION_WAITING: &str = "Grapt: question waiting";

/// The terminal notification transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// `OSC 9` carrying the line as notification text.
    Osc9,
    /// A bare bell.
    Bel,
}

/// A notification is due when the user is not looking: the terminal lost
/// focus, or the user has been idle for `IDLE_AFTER`.
pub fn due(focused: bool, idle: bool) -> bool {
    !focused || idle
}

/// The exact bytes `method` emits for `line`: OSC 9 carries the line, the
/// bell carries nothing.
pub fn bytes(method: Method, line: &str) -> Vec<u8> {
    match method {
        Method::Osc9 => format!("\x1b]9;{line}\x07").into_bytes(),
        Method::Bel => vec![0x07],
    }
}

/// tmux does not forward OSC 9; wrap it in the DCS passthrough envelope so
/// the client terminal receives it. Every escape byte in `payload` is
/// doubled so the payload cannot terminate the envelope early.
pub fn tmux_passthrough(payload: &[u8]) -> Vec<u8> {
    let mut out = b"\x1bPtmux;".to_vec();
    for &byte in payload {
        if byte == 0x1b {
            out.push(0x1b);
        }
        out.push(byte);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Terminals receive the OSC 9 bell when their directory, version, session or
/// window environment is detected; every other terminal takes the bell.
pub fn osc9_for(
    term_program: &str,
    ghostty_dir: bool,
    wezterm_version: bool,
    iterm_session: bool,
    kitty_window: bool,
) -> bool {
    if term_program.len() > 128 {
        return ghostty_dir || wezterm_version || iterm_session || kitty_window;
    }
    let normalized: String = term_program
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '_' | '.'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    matches!(
        normalized.as_str(),
        "ghostty" | "iterm" | "iterm2" | "itermapp" | "warp" | "warpterminal" | "wezterm" | "kitty"
    ) || ghostty_dir
        || wezterm_version
        || iterm_session
        || kitty_window
}

/// Remembers the last observed (selected session, running) pair and reports
/// the running-to-idle edge of the same session. A session switch never
/// reads as a completion of the previous session's turn.
#[derive(Debug, Default)]
pub struct TurnEdge {
    selected: Option<SessionWireId>,
    running: bool,
}

impl TurnEdge {
    pub fn observe(&mut self, selected: Option<SessionWireId>, running: Option<bool>) -> bool {
        let same = self
            .selected
            .as_ref()
            .zip(selected.as_ref())
            .is_some_and(|(previous, current)| previous == current);
        if !same {
            self.running = false;
        }
        self.selected = selected;
        let Some(running) = running else {
            return false;
        };
        let completed = same && self.running && !running;
        self.running = running;
        completed
    }
}

/// Emits notifications on stdout once the state proves the user needs them.
/// A failed write disables later notifications instead of spilling errors
/// into the session.
#[derive(Debug)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub struct Notifier {
    allowed: bool,
    enabled: bool,
    configured: bool,
    method: Method,
    tmux: bool,
    focused: bool,
    last_input: Duration,
    turn: TurnEdge,
    approval: Option<(SessionWireId, PermissionWireId)>,
    questions: Vec<(FormOwner, FormWireId)>,
}

impl Notifier {
    pub fn new(enabled: bool) -> Self {
        Self {
            allowed: enabled,
            enabled,
            configured: enabled,
            method: Self::auto_method(),
            tmux: std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some(),
            focused: true,
            last_input: Duration::ZERO,
            turn: TurnEdge::default(),
            approval: None,
            questions: Vec::new(),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        let enabled = self.allowed && enabled;
        if self.configured != enabled {
            self.configured = enabled;
            self.enabled = enabled;
            self.turn = TurnEdge::default();
            self.approval = None;
            self.questions.clear();
        }
    }

    pub fn input(&mut self, event: &Event, now: Duration) {
        match event {
            Event::FocusLost => self.focused = false,
            Event::FocusGained => {
                self.focused = true;
                self.last_input = now;
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => self.last_input = now,
            Event::Paste(_) => self.last_input = now,
            _ => {}
        }
    }

    pub fn update(&mut self, update: &crate::backend::Update, now: Duration) {
        if !self.enabled || !update.connected {
            return;
        }
        for line in self
            .lines(
                update.selected.as_ref(),
                update.observed_running,
                update.permission.as_ref(),
                &update.forms,
                now,
            )
            .into_iter()
            .flatten()
        {
            self.emit(line);
        }
    }

    fn lines(
        &mut self,
        selected: Option<&SessionWireId>,
        running: Option<bool>,
        permission: Option<&PermissionAsk>,
        forms: &[FormRow],
        now: Duration,
    ) -> [Option<&'static str>; 3] {
        if self.turn.selected.as_ref() != selected {
            self.questions.clear();
        }
        let approval = permission
            // R displays the first live approval across all sessions. Notify
            // for that visible ask even while another transcript is selected.
            .filter(|ask| ask.live)
            .map(|ask| (ask.session_id.clone(), ask.request_id.clone()));
        let approval_arrived = approval.is_some() && approval != self.approval;
        self.approval = approval;
        // Match the visible question board's bound. These are advisory edge
        // keys only; the existing Client and Board still own question state.
        let questions: Vec<_> = forms
            .iter()
            .filter(|row| match &row.owner {
                FormOwner::Session(session) => Some(session) == selected,
                FormOwner::Global => true,
                FormOwner::Unattributed(_) => false,
            })
            .take(grapt_tui_forms::MAX_FORMS)
            .map(|row| (row.owner.clone(), row.id.clone()))
            .collect();
        let question_arrived = questions.iter().any(|key| !self.questions.contains(key));
        let waiting = self.approval.is_some() || !questions.is_empty();
        self.questions = questions;
        let completed = self.turn.observe(selected.cloned(), running)
            && !waiting
            && due(
                self.focused,
                now.saturating_sub(self.last_input) >= IDLE_AFTER,
            );
        [
            completed.then_some(TURN_COMPLETE),
            approval_arrived.then_some(APPROVAL_WAITING),
            question_arrived.then_some(QUESTION_WAITING),
        ]
    }

    /// OSC 9 where a supported terminal advertises itself, the bell
    /// everywhere else.
    pub fn auto_method() -> Method {
        let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
        let term = std::env::var("TERM").unwrap_or_default();
        let kitty = std::env::var_os("KITTY_WINDOW_ID").is_some()
            || term.len() <= 128 && term.contains("kitty");
        if osc9_for(
            &term_program,
            std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some(),
            std::env::var_os("WEZTERM_VERSION").is_some(),
            std::env::var_os("ITERM_SESSION_ID").is_some()
                || std::env::var_os("ITERM_PROFILE").is_some()
                || std::env::var_os("ITERM_PROFILE_NAME").is_some(),
            kitty,
        ) {
            Method::Osc9
        } else {
            Method::Bel
        }
    }

    pub fn emit(&mut self, line: &str) {
        let stdout = std::io::stdout();
        if self.emit_to(line, &mut stdout.lock()).is_err() {
            eprintln!("grapt-tui: terminal notification failed; notifications disabled");
        }
    }

    pub(crate) fn emit_to(&mut self, line: &str, output: &mut impl Write) -> std::io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let mut payload = bytes(self.method, line);
        if self.method == Method::Osc9 && self.tmux {
            payload = tmux_passthrough(&payload);
        }
        let result = output.write_all(&payload).and_then(|_| output.flush());
        if result.is_err() {
            self.enabled = false;
        }
        result
    }
}

#[cfg(test)]
mod tests;
