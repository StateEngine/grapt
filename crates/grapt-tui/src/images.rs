//! Removable draft image rows for the composer.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Modified by Grapt: four bounded inline attachments and submission identities.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_api::u3a::PromptInputFileAttachmentDto;
use ratatui::{
    layout::{Position, Rect},
    widgets::Paragraph,
    Frame,
};
use std::path::{Path, PathBuf};

pub struct Attachment {
    pub file: PromptInputFileAttachmentDto,
    label: String,
}
pub fn prepared(name: String, image: grapt_image::PreparedImage) -> Result<Attachment, String> {
    let label = format!("{} · {}×{}", name, image.width, image.height);
    let file = serde_json::from_value(serde_json::json!({"uri":image.data_url(),"name":name}))
        .map_err(|error| format!("Image attachment: {error}"))?;
    Ok(Attachment { file, label })
}
pub fn from_path(path: &Path) -> Result<Attachment, String> {
    let image = grapt_image::read_path(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Image".into());
    prepared(name, image)
}
pub fn pasted_path(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    let quoted = (text.starts_with('"') && text.ends_with('"'))
        || (text.starts_with('\'') && text.ends_with('\''));
    let text = text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| text.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(text);
    if text.contains(['\n', '\r']) {
        return None;
    }
    let path = PathBuf::from(text);
    if !quoted
        && text.chars().any(char::is_whitespace)
        && !path.is_absolute()
        && !text.starts_with(['.', '\\'])
        && text.as_bytes().get(1) != Some(&b':')
    {
        return None;
    }
    matches!(
        path.extension()?.to_str()?.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp"
    )
    .then_some(path)
}

pub struct Prompt {
    pub text: String,
    pub files: Vec<PromptInputFileAttachmentDto>,
}
impl std::fmt::Debug for Prompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImagePrompt")
            .field("images", &self.files.len())
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    Accepted,
    Unconfirmed,
    Rejected,
}
#[derive(Default)]
pub struct Images {
    items: Vec<(u64, Attachment)>,
    serial: u64,
    revision: u64,
    /// The submitted images, and the command that carried them once sent.
    submitted: Option<(Option<crate::backend::CommandId>, Vec<u64>)>,
    uncertain: Option<(crate::backend::CommandId, Vec<u64>)>,
    focus: Option<usize>,
    area: Rect,
}
impl Images {
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn restore(
        &mut self,
        files: Vec<PromptInputFileAttachmentDto>,
    ) -> Result<(), &'static str> {
        if !self.items.is_empty() || files.len() > 4 {
            return Err("Keep the current images before editing a queued follow-up");
        }
        self.serial
            .checked_add(files.len() as u64)
            .ok_or("Image identity exhausted")?;
        for file in files {
            self.add(Attachment {
                file,
                label: "Restored image".into(),
            })?;
        }
        Ok(())
    }
    pub fn add(&mut self, image: Attachment) -> Result<(), &'static str> {
        if self.items.len() == 4 {
            return Err("A prompt supports at most four images");
        }
        self.serial = self
            .serial
            .checked_add(1)
            .ok_or("Image identity exhausted")?;
        self.items.push((self.serial, image));
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
    pub fn submission(&mut self, text: String) -> Result<Prompt, &'static str> {
        self.submitted = Some((None, self.items.iter().map(|(id, _)| *id).collect()));
        Ok(Prompt {
            text,
            files: self.items.iter().map(|(_, i)| i.file.clone()).collect(),
        })
    }
    /// The current submission went out as command `id`.
    pub fn sent(&mut self, id: crate::backend::CommandId) {
        if let Some((owner @ None, _)) = &mut self.submitted {
            *owner = Some(id);
        }
    }
    /// Settle the images sent as command `id`; nothing else can match.
    pub fn settle(&mut self, id: crate::backend::CommandId, result: Settlement) -> bool {
        let snapshot = if self
            .submitted
            .as_ref()
            .is_some_and(|(owner, _)| *owner == Some(id))
        {
            self.submitted.take().map(|(_, items)| items)
        } else if self
            .uncertain
            .as_ref()
            .is_some_and(|(owner, _)| *owner == id)
        {
            self.uncertain.take().map(|(_, items)| items)
        } else {
            None
        };
        snapshot.is_some_and(|items| self.finish(id, items, result))
    }
    /// Settle a submission that never left this window, such as images
    /// queued with a follow-up.
    pub fn settle_local(&mut self, result: Settlement) -> bool {
        match self.submitted.take() {
            Some((None, items)) => match result {
                Settlement::Accepted => self.accept(&items),
                Settlement::Unconfirmed | Settlement::Rejected => false,
            },
            other => {
                self.submitted = other;
                false
            }
        }
    }
    fn finish(
        &mut self,
        id: crate::backend::CommandId,
        items: Vec<u64>,
        result: Settlement,
    ) -> bool {
        match result {
            Settlement::Unconfirmed => {
                self.uncertain = Some((id, items));
                false
            }
            Settlement::Rejected => false,
            Settlement::Accepted => self.accept(&items),
        }
    }
    fn accept(&mut self, items: &[u64]) -> bool {
        self.items.retain(|(id, _)| !items.contains(id));
        self.focus = None;
        self.revision = self.revision.wrapping_add(1);
        true
    }
    fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
            self.focus = None;
            self.revision = self.revision.wrapping_add(1);
        }
    }
    pub fn key(&mut self, key: KeyEvent, at_start: bool) -> bool {
        if self.items.is_empty()
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return false;
        }
        if let Some(index) = self.focus {
            match key.code {
                KeyCode::Delete | KeyCode::Backspace => self.remove(index),
                KeyCode::Up => self.focus = Some(index.saturating_sub(1)),
                KeyCode::Down if index + 1 < self.items.len() => self.focus = Some(index + 1),
                KeyCode::Down | KeyCode::Esc => self.focus = None,
                _ => {
                    self.focus = None;
                    return false;
                }
            }
            self.revision = self.revision.wrapping_add(1);
            return true;
        }
        if at_start && key.code == KeyCode::Up {
            self.focus = Some(self.items.len() - 1);
            self.revision = self.revision.wrapping_add(1);
            return true;
        }
        false
    }
    pub fn click(&mut self, column: u16, row: u16) -> bool {
        if !self.area.contains(Position::new(column, row)) {
            return false;
        }
        let index = usize::from(row - self.area.y);
        if column >= self.area.right().saturating_sub(2) {
            self.remove(index);
        } else {
            self.focus = Some(index);
            self.revision = self.revision.wrapping_add(1);
        }
        true
    }
    pub fn paint(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        palette: grapt_tui_transcript::palette::Palette,
    ) {
        self.area = Rect {
            height: area.height.min(self.items.len() as u16),
            ..area
        };
        for (index, (_, image)) in self
            .items
            .iter()
            .take(usize::from(self.area.height))
            .enumerate()
        {
            let label = grapt_tui_transcript::markdown::literal(&image.label).replace('\n', " ");
            let line = format!("[Image {}] {}", index + 1, label);
            let line = grapt_client::cells::cut(&line, usize::from(area.width.saturating_sub(3)));
            let style = palette.normal().fg(if self.focus == Some(index) {
                palette.accent
            } else {
                palette.muted
            });
            let rect = Rect {
                y: area.y + index as u16,
                height: 1,
                ..area
            };
            frame.render_widget(Paragraph::new(line).style(style), rect);
            frame.render_widget(
                Paragraph::new("×").style(style),
                Rect {
                    x: area.right().saturating_sub(2),
                    width: 2.min(area.width),
                    ..rect
                },
            );
        }
    }
}
