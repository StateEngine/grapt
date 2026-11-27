// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified by Grapt: bounded queues, existing wake/clock, session-scoped paste.
#[cfg(target_os = "linux")]
pub(crate) mod linux;
pub(crate) mod read;

use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};

pub enum Action {
    Copy(String),
    Paste,
    PastedPath {
        path: std::path::PathBuf,
        text: String,
    },
    Attach {
        path: std::path::PathBuf,
        draft: Option<String>,
    },
}

#[derive(Default)]
pub struct State {
    epoch: u64,
    pending: Option<Action>,
    hint: Option<(String, Duration)>,
}

impl State {
    pub fn reset(&mut self) {
        *self = Self {
            epoch: self.epoch.saturating_add(1),
            ..Self::default()
        };
    }
    pub fn request(&mut self, action: Action) {
        self.epoch = self.epoch.saturating_add(1);
        self.pending = Some(action);
    }
    pub fn take_request(&mut self) -> Option<Action> {
        self.pending.take()
    }
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_ref().map(|(text, _)| text.as_str())
    }
    fn notice(&mut self, text: &str, now: Duration) {
        self.hint = Some((text.into(), now.saturating_add(Duration::from_secs(2))));
    }
    pub fn tick(&mut self, now: Duration) -> bool {
        if self.hint.as_ref().is_some_and(|(_, end)| now >= *end) {
            self.hint = None;
            true
        } else {
            false
        }
    }
}

struct Request {
    epoch: u64,
    action: Action,
}
enum Content {
    Text(String),
    Literal(String),
    Image(crate::images::Attachment, Option<String>),
}
struct Reply {
    epoch: u64,
    result: Result<Option<Content>, String>,
}

pub struct Worker {
    requests: SyncSender<Request>,
    replies: Receiver<Reply>,
    busy: bool,
    pasting: bool,
}

impl Worker {
    pub fn start(wake: SyncSender<()>) -> Result<Self, String> {
        let (requests, receiver) = mpsc::sync_channel::<Request>(1);
        let (sender, replies) = mpsc::sync_channel(1);
        let _worker = std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                let mut platform = Platform::new();
                while let Ok(request) = receiver.recv() {
                    let result = platform.perform(request.action);
                    if sender
                        .send(Reply {
                            epoch: request.epoch,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    let _ = wake.try_send(());
                }
            })
            .map_err(|_| "Could not start clipboard worker")?;
        Ok(Self {
            requests,
            replies,
            busy: false,
            pasting: false,
        })
    }

    pub fn poll(&mut self, ui: &mut crate::ui::Ui, now: Duration) -> bool {
        let mut changed = ui.clipboard.tick(now);
        if self.pasting
            && ((!ui.mentions_enabled()
                && !(ui.connect.accepts_input() && ui.permission.is_none()))
                || ui.quit.waiting())
        {
            self.pasting = false;
            ui.clipboard.epoch = ui.clipboard.epoch.saturating_add(1);
            ui.clipboard
                .notice("Paste cancelled: composer is not active", now);
            changed = true;
        }
        let reply = match self.replies.try_recv() {
            Ok(reply) => Some(reply),
            Err(mpsc::TryRecvError::Disconnected) if self.busy => {
                self.busy = false;
                self.pasting = false;
                ui.clipboard.notice("Clipboard unavailable", now);
                changed = true;
                None
            }
            Err(_) => None,
        };
        if let Some(reply) = reply {
            self.busy = false;
            self.pasting = false;
            if reply.epoch == ui.clipboard.epoch {
                match reply.result {
                    Ok(None) => ui.clipboard.notice("Copied", now),
                    Ok(Some(content))
                        if (ui.mentions_enabled()
                            || (ui.connect.accepts_input() && ui.permission.is_none()))
                            && !ui.quit.waiting() =>
                    {
                        match content {
                            Content::Text(text) => {
                                crate::keys::paste(&text, ui);
                            }
                            Content::Literal(text) => {
                                if ui.connect.visible() {
                                    ui.connect.paste(&text);
                                } else {
                                    crate::keys::insert_paste(&text, ui);
                                }
                            }
                            Content::Image(_, _) if ui.connect.visible() => {
                                ui.clipboard.notice("Paste a text credential here", now)
                            }
                            Content::Image(image, draft) => match ui.images.add(image) {
                                Ok(()) => {
                                    if let Some(draft) = draft {
                                        ui.recall.replace_if_unchanged(
                                            &draft,
                                            "",
                                            &mut ui.composer,
                                        );
                                    }
                                    ui.clipboard
                                        .notice("Image attached; Up at start selects it", now);
                                }
                                Err(error) => ui.clipboard.notice(error, now),
                            },
                        }
                    }
                    Ok(Some(_)) => ui
                        .clipboard
                        .notice("Paste cancelled: composer is not active", now),
                    Err(error) => ui.clipboard.notice(&error, now),
                }
                changed = true;
            }
        }
        if let Some(action) = ui.clipboard.take_request() {
            if self.busy {
                ui.clipboard.notice("Clipboard busy; try again", now);
            } else {
                self.pasting = matches!(
                    &action,
                    Action::Paste | Action::Attach { .. } | Action::PastedPath { .. }
                );
                if self
                    .requests
                    .try_send(Request {
                        epoch: ui.clipboard.epoch,
                        action,
                    })
                    .is_ok()
                {
                    self.busy = true;
                } else {
                    self.pasting = false;
                    ui.clipboard.notice("Clipboard unavailable", now);
                }
            }
            changed = true;
        }
        changed
    }
}

#[cfg(not(windows))]
#[derive(Default)]
struct Platform {
    clipboard: Option<arboard::Clipboard>,
}

#[cfg(not(windows))]
impl Platform {
    fn new() -> Self {
        Self::default()
    }
    fn text(&mut self, action: Action) -> Result<Option<String>, &'static str> {
        match action {
            Action::Attach { .. } | Action::PastedPath { .. } => {
                Err("Invalid text clipboard request")
            }
            Action::Paste => read::read_text().and_then(paste_text).map(Some),
            Action::Copy(text) => {
                if self.clipboard.is_none() {
                    self.clipboard =
                        Some(arboard::Clipboard::new().map_err(|_| "Clipboard unavailable")?);
                }
                self.clipboard
                    .as_mut()
                    .ok_or("Clipboard unavailable")?
                    .set_text(text)
                    .map(|_| None)
                    .map_err(|_| "Copy failed")
            }
        }
    }
}

#[cfg(windows)]
#[derive(Default)]
struct Platform;

#[cfg(windows)]
impl Platform {
    fn new() -> Self {
        Self
    }
    fn text(&mut self, action: Action) -> Result<Option<String>, &'static str> {
        use clipboard_win::{formats::CF_UNICODETEXT, raw, Clipboard};
        let _clipboard = Clipboard::new_attempts(5).map_err(|_| "Clipboard busy; try again")?;
        match action {
            Action::Attach { .. } | Action::PastedPath { .. } => {
                Err("Invalid text clipboard request")
            }
            Action::Copy(text) => raw::set_string(&text)
                .map(|_| None)
                .map_err(|_| "Copy failed"),
            Action::Paste => {
                let size = raw::size(CF_UNICODETEXT)
                    .ok_or("Clipboard has no readable text")?
                    .get();
                if size > crate::MAX_DRAFT * 2 + 2 {
                    return Err("Clipboard exceeds 64 KiB; paste refused");
                }
                let mut bytes = vec![0u8; size];
                let count = raw::get(CF_UNICODETEXT, &mut bytes)
                    .map_err(|_| "Clipboard has no readable text")?;
                let units = bytes[..count]
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .take_while(|unit| *unit != 0)
                    .collect::<Vec<_>>();
                let text = String::from_utf16(&units)
                    .map_err(|_| "Clipboard text is not valid Unicode")?;
                paste_text(text).map(Some)
            }
        }
    }
}

fn paste_text(text: String) -> Result<String, &'static str> {
    if text.len() > crate::MAX_DRAFT {
        return Err("Clipboard exceeds 64 KiB; paste refused");
    }
    Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
}

impl Platform {
    fn perform(&mut self, action: Action) -> Result<Option<Content>, String> {
        match action {
            Action::PastedPath { path, text } => Ok(Some(match crate::images::from_path(&path) {
                Ok(image) => Content::Image(image, None),
                Err(_) => Content::Literal(text),
            })),
            Action::Attach { path, draft } => crate::images::from_path(&path)
                .map(|image| Content::Image(image, draft))
                .map(Some),
            Action::Paste => {
                if let Some(bytes) = read::read_image().map_err(str::to_owned)? {
                    let image = grapt_image::prepare(bytes, grapt_image::Detail::High)?;
                    return crate::images::prepared("Clipboard image".into(), image)
                        .map(|image| Content::Image(image, None))
                        .map(Some);
                }
                self.text(Action::Paste)
                    .map(|text| text.map(Content::Text))
                    .map_err(str::to_owned)
            }
            Action::Copy(text) => self
                .text(Action::Copy(text))
                .map(|text| text.map(Content::Text))
                .map_err(str::to_owned),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_paste_and_failed_automatic_paths_preserve_the_original_text() -> Result<(), String>
    {
        let mut ui = crate::ui::Ui::new(false);
        let prose = "please inspect screenshot.png";
        assert!(crate::keys::paste(prose, &mut ui));
        assert_eq!(crate::keys::draft(&ui), prose);
        assert!(ui.clipboard.take_request().is_none());
        let missing = "\"missing-image-input-proof.png\"";
        assert!(crate::keys::paste(missing, &mut ui));
        let action = ui
            .clipboard
            .take_request()
            .ok_or("automatic path request")?;
        let mut platform = Platform::new();
        let Some(Content::Literal(text)) = platform.perform(action)? else {
            return Err("failed path lost original paste".into());
        };
        assert!(crate::keys::insert_paste(&text, &mut ui));
        assert_eq!(crate::keys::draft(&ui), format!("{prose}{missing}"));
        assert!(platform
            .perform(Action::Attach {
                path: "missing-image-input-proof.png".into(),
                draft: Some("/attach missing-image-input-proof.png".into())
            })
            .is_err());
        Ok(())
    }
}
