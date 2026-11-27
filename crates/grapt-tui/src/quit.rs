//! Timed quit confirmation for the native backend.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Grapt: injected elapsed time and an acknowledged normal Session interrupt.
use crate::{backend::Command, keys, send, ui::Ui, Backend};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::{sync::mpsc, time::Duration};

const CONFIRM: Duration = Duration::from_millis(1500);
// Quit may wait for an earlier resume stop, then confirm its own stop.
// Include both 60-second requests, identity preflights and reply delivery.
const STOP_TIMEOUT: Duration = Duration::from_secs(135);
// An unavoidable close gets a bounded window to confirm its stop, on every
// platform that can report one: a Windows console event or a Unix signal.
const CONSOLE_GRACE: Duration = Duration::from_secs(4);
pub const HINT: &str = "Press Ctrl+C again to exit";

#[derive(Default)]
pub struct Quit {
    armed: Option<Duration>,
    unattached: bool,
    local_exit: bool,
    closing: Option<Duration>,
    stopping: Option<(Duration, mpsc::Receiver<Result<(), String>>)>,
}

impl Quit {
    pub fn hint(&self) -> Option<&'static str> {
        if self.waiting() {
            Some("Stopping session before exit…")
        } else {
            self.armed.map(|_| HINT)
        }
    }

    pub fn waiting(&self) -> bool {
        self.local_exit || self.stopping.is_some() || self.closing.is_some()
    }

    pub fn disarm(&mut self) {
        self.armed = None;
    }

    pub fn tick(&mut self, now: Duration) -> bool {
        if self.armed.is_some_and(|until| now >= until) {
            self.disarm();
            true
        } else {
            false
        }
    }

    pub fn backend_failed(&mut self, unattached: bool) {
        self.unattached = unattached;
    }

    pub fn request(&mut self, backend: &Backend, now: Duration) -> Result<(), String> {
        if self.unattached {
            self.disarm();
            self.local_exit = true;
            return Ok(());
        }
        if self.stopping.is_none() {
            let (reply, result) = mpsc::sync_channel(1);
            send(backend, Command::Quit(reply))?;
            self.disarm();
            self.stopping = Some((now.saturating_add(STOP_TIMEOUT), result));
        }
        Ok(())
    }

    /// An unavoidable close may terminate the process once its grace expires,
    /// so repeated notifications must not extend that deadline. The same path
    /// serves a Windows console event and a Unix hangup or termination.
    pub fn close_console(&mut self, backend: &Backend, now: Duration) {
        self.closing
            .get_or_insert(now.saturating_add(CONSOLE_GRACE));
        self.disarm();
        // Enqueue before touching console I/O, which may already be unavailable.
        // Admission failures are retried by poll within the original deadline.
        let _ = self.request(backend, now);
    }

    pub fn closing(&self) -> bool {
        self.closing.is_some()
    }

    pub fn poll(&mut self, backend: &Backend, now: Duration) -> Option<Result<(), String>> {
        if std::mem::take(&mut self.local_exit) {
            return Some(Ok(()));
        }
        if let Some(until) = self.closing {
            if now >= until {
                self.stopping = None;
                return Some(Err(
                    "Console closing: session stop acknowledgement timed out".into(),
                ));
            }
            // Queue pressure can clear while the UI drains protocol updates.
            // Retry only enqueueing; an accepted request is never sent again.
            if self.stopping.is_none() && self.request(backend, now).is_err() {
                return None;
            }
        }
        let (until, reply) = self.stopping.as_ref()?;
        let result = match reply.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) if now < *until => return None,
            Err(mpsc::TryRecvError::Empty) => {
                Err("Stopping session timed out; terminal kept open".into())
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Session stop was not confirmed; terminal kept open".into())
            }
        };
        self.stopping = None;
        Some(result)
    }
}

pub fn console_interrupt(ui: &mut Ui, backend: &Backend, now: Duration) {
    ui.quit.disarm();
    let event = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let _ = key(event, ui, backend, now);
}

/// Consume Ctrl+C before overlay routing. Repeat events never confirm a quit.
pub fn key(key: KeyEvent, ui: &mut Ui, backend: &Backend, now: Duration) -> Option<bool> {
    if ui.quit.waiting() {
        return Some(false);
    }
    let control_c = matches!(key.code, KeyCode::Char('c' | 'C'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::ALT);
    if !control_c {
        ui.quit.disarm();
        return None;
    }
    if key.kind != KeyEventKind::Press {
        return Some(false);
    }
    if ui.connect.visible() {
        ui.clipboard.reset();
        ui.connect
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        return Some(false);
    }
    if ui.command_palette.take().is_some() || ui.settings.take().is_some() {
        ui.clipboard.reset();
        return Some(false);
    }
    ui.launch.cancel();
    if ui.quit.armed.is_some_and(|until| now < until) {
        ui.quit.disarm();
        return Some(true);
    }
    ui.quit.armed = Some(now.saturating_add(CONFIRM));
    if ui.views.loading() {
        return Some(false);
    }
    if ui.running() || ui.pending.is_some() {
        ui.followups.pause();
        if let Err(error) = send(backend, Command::Interrupt(ui.live_execution.clone())) {
            ui.status = error;
        }
    } else {
        ui.recall.cancel(&mut ui.composer);
        if !keys::draft(ui).is_empty() {
            ui.composer.edit(|area| {
                area.clear();
            });
            ui.recall.edited();
        }
    }
    Some(false)
}

#[cfg(test)]
mod tests;
