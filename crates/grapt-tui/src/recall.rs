//! Adapt shared bounded history to the editor; recall never submits a prompt.
//! Copyright 2025 OpenAI (licenses/openai-codex/).
use crate::editor::Editor as TextArea;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::{
    composer_history::{Draft, History, SearchStatus},
    wire::SessionWireId,
};
use grapt_tui_transcript::transcript::Transcript;
use ratatui_textarea::CursorMove;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub struct Recall {
    history: History,
    original: Option<TextArea<'static>>,
    session: Option<SessionWireId>,
    notice: Option<&'static str>,
}

impl Recall {
    pub fn searching(&self) -> bool {
        self.history.searching()
    }

    pub fn session(
        &mut self,
        session: Option<SessionWireId>,
        editor: &mut TextArea<'static>,
    ) -> bool {
        if self.session == session {
            return false;
        }
        self.cancel(editor);
        *self = Self {
            session,
            ..Self::default()
        };
        true
    }

    pub fn edited(&mut self) {
        self.history.edited();
    }

    pub fn submitted(&mut self, text: &str, editor: &mut TextArea<'static>) -> bool {
        self.replace_if_unchanged(text, "", editor)
    }

    /// Complete an asynchronous command in the original draft, even while a
    /// history-search preview is visible. A concurrent edit keeps its snapshot.
    pub fn replace_if_unchanged(
        &mut self,
        text: &str,
        replacement: &str,
        editor: &mut TextArea<'static>,
    ) -> bool {
        if let Some(original) = &mut self.original {
            if original.lines().join("\n") == text {
                *original = empty();
                original.insert_str(replacement);
                if self.history.selected().is_none() {
                    *editor = original.clone();
                    return true;
                }
            }
            return false;
        }
        if editor.lines().join("\n") != text {
            return false;
        }
        *editor = empty();
        editor.insert_str(replacement);
        self.history = History::default();
        true
    }

    pub fn paste(&mut self, text: &str, editor: &mut TextArea<'static>) {
        let mut query = self.history.query().unwrap_or_default().to_owned();
        query.push_str(text);
        if self.history.set_query(query) {
            self.notice = None;
            self.preview(editor);
        } else {
            self.notice = Some("query exceeds 256 bytes");
        }
    }

    pub fn key(
        &mut self,
        key: KeyEvent,
        transcript: &Transcript,
        editor: &mut TextArea<'static>,
    ) -> bool {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.searching() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || control => {
                    self.cancel(editor)
                }
                KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    let selected = self.history.selected();
                    self.history.accept_search();
                    if let Some(selected) = selected {
                        replace(editor, selected);
                        self.original = None;
                    } else if let Some(original) = self.original.take() {
                        *editor = original;
                    }
                }
                KeyCode::Up | KeyCode::Char('r') if key.code == KeyCode::Up || control => {
                    self.history.search_older();
                    self.preview(editor);
                }
                KeyCode::Down | KeyCode::Char('s') if key.code == KeyCode::Down || control => {
                    self.history.search_newer();
                    self.preview(editor);
                }
                KeyCode::Backspace => {
                    let mut query = self.history.query().unwrap_or_default().to_owned();
                    let end = query
                        .grapheme_indices(true)
                        .next_back()
                        .map_or(0, |(byte, _)| byte);
                    query.truncate(end);
                    self.history.set_query(query);
                    self.notice = None;
                    self.preview(editor);
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.paste(&ch.to_string(), editor)
                }
                KeyCode::Enter
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::Delete => {}
                _ => {
                    self.cancel(editor);
                    return false;
                }
            }
            return true;
        }
        if (control && key.code == KeyCode::Char('r'))
            || (key.modifiers.is_empty() && matches!(key.code, KeyCode::Up | KeyCode::Down))
        {
            self.history.refresh(transcript.prompts());
            let text = editor.lines().join("\n");
            let ratatui_textarea::DataCursor(row, column) = editor.cursor();
            let cursor = editor
                .lines()
                .iter()
                .take(row)
                .map(|line| line.len() + 1)
                .sum::<usize>()
                + editor.lines().get(row).map_or(0, |line| {
                    line.char_indices()
                        .nth(column)
                        .map_or(line.len(), |(byte, _)| byte)
                });
            if control && key.code == KeyCode::Char('r') {
                self.original = Some(std::mem::replace(editor, empty()));
                self.notice = None;
                if let Some(draft) = self.history.begin_search(&text, cursor) {
                    replace(editor, draft);
                } else {
                    self.preview(editor);
                }
                return true;
            }
            let navigating = self.history.should_navigate(&text, cursor);
            let selected = if key.code == KeyCode::Up {
                self.history.older(&text, cursor)
            } else {
                self.history.newer(&text, cursor)
            };
            if let Some(draft) = selected {
                replace(editor, draft);
            }
            return navigating;
        }
        false
    }

    pub fn status(&self) -> Option<String> {
        let query = self.history.query()?;
        let status = self.notice.unwrap_or(match self.history.status() {
            Some(SearchStatus::NoMatch) => "no match",
            Some(SearchStatus::Boundary) => "boundary",
            _ => "match",
        });
        Some(format!(
            "Loaded history {status} · Enter edit · Esc cancel │ {} · Ctrl+R/↑ older · ↓ newer",
            grapt_tui_transcript::markdown::literal(query).replace('\n', "\\n")
        ))
    }

    fn preview(&self, editor: &mut TextArea<'static>) {
        if let Some(draft) = self.history.selected() {
            replace(editor, draft);
        } else if let Some(original) = &self.original {
            *editor = original.clone();
        }
    }

    pub(crate) fn cancel(&mut self, editor: &mut TextArea<'static>) {
        let draft = self.history.cancel();
        if let Some(original) = self.original.take() {
            *editor = original;
        } else if let Some(draft) = draft {
            replace(editor, draft);
        }
    }
}

fn empty() -> TextArea<'static> {
    let mut editor = TextArea::default();
    editor.set_max_histories(128);
    editor.set_wrap_mode(ratatui_textarea::WrapMode::None);
    editor
}

fn replace(editor: &mut TextArea<'static>, draft: Draft) {
    *editor = empty();
    editor.insert_str(&draft.text);
    let prefix = &draft.text[..draft.cursor.min(draft.text.len())];
    let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix
        .rsplit('\n')
        .next()
        .map_or(0, |line| line.chars().count());
    editor.move_cursor(CursorMove::Jump(
        row.min(u16::MAX as usize) as u16,
        column.min(u16::MAX as usize) as u16,
    ));
}

#[cfg(test)]
mod tests;
