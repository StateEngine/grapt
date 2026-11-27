//! Composer edits have one bounded undo owner, including completion replacement.
use ratatui_textarea::{CursorMove, Input, Key, TextArea, WrapMode};
use std::{collections::VecDeque, ops::Deref};

const MAX_EDITS: usize = 128;
const MAX_HISTORY_BYTES: usize = 4 * 1024 * 1024;

// Store text contiguously: one String per line amplifies multiline history.
#[derive(Clone)]
struct Snapshot {
    text: String,
    cursor: (usize, usize),
}
impl Snapshot {
    fn capture(area: &TextArea<'_>) -> Self {
        Self {
            text: area.lines().join("\n"),
            cursor: {
                let ratatui_textarea::DataCursor(row, col) = area.cursor();
                (row, col)
            },
        }
    }
    fn bytes(&self) -> usize {
        self.text.capacity()
    }
}

#[derive(Clone)]
pub struct Editor<'a> {
    area: TextArea<'a>,
    undo: VecDeque<Snapshot>,
    redo: VecDeque<Snapshot>,
    limit: usize,
}
impl Default for Editor<'_> {
    fn default() -> Self {
        Self::new(vec![String::new()])
    }
}
impl<'a> Deref for Editor<'a> {
    type Target = TextArea<'a>;
    fn deref(&self) -> &Self::Target {
        &self.area
    }
}
impl<'a> Editor<'a> {
    pub fn new(lines: Vec<String>) -> Self {
        let mut area = TextArea::new(lines);
        // Native selection replacement records two edits. Snapshot transactions
        // retain the complete draft/cursor and keep only one undo stack.
        area.set_max_histories(0);
        Self {
            area,
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            limit: MAX_EDITS,
        }
    }
    pub fn set_max_histories(&mut self, limit: usize) {
        self.limit = limit.min(MAX_EDITS);
        self.undo.clear();
        self.redo.clear();
    }
    pub fn set_wrap_mode(&mut self, mode: WrapMode) {
        self.area.set_wrap_mode(mode);
    }
    pub fn move_cursor(&mut self, movement: CursorMove) {
        self.area.move_cursor(movement);
    }
    pub fn edit(&mut self, apply: impl FnOnce(&mut TextArea<'a>)) -> bool {
        let before = Snapshot::capture(&self.area);
        apply(&mut self.area);
        if self
            .area
            .lines()
            .iter()
            .map(String::as_str)
            .eq(before.text.split('\n'))
        {
            return false;
        }
        self.redo.clear();
        if self.limit > 0 {
            self.undo.push_back(before);
            self.trim_history();
        }
        true
    }
    pub fn insert_str(&mut self, text: impl AsRef<str>) -> bool {
        self.edit(|area| {
            area.insert_str(text);
        })
    }
    pub fn input(&mut self, input: impl Into<Input>) -> bool {
        let input = input.into();
        match input {
            Input {
                key: Key::Char('u'),
                ctrl: true,
                alt: false,
                ..
            } => self.undo(),
            Input {
                key: Key::Char('r'),
                ctrl: true,
                alt: false,
                ..
            } => self.redo(),
            _ => self.edit(|area| {
                area.input(input);
            }),
        }
    }
    pub fn undo(&mut self) -> bool {
        let Some(before) = self.undo.pop_back() else {
            return false;
        };
        let current = self.restore(before);
        self.redo.push_back(current);
        self.trim_history();
        true
    }
    pub fn redo(&mut self) -> bool {
        let Some(after) = self.redo.pop_back() else {
            return false;
        };
        let current = self.restore(after);
        self.undo.push_back(current);
        self.trim_history();
        true
    }
    fn history_bytes(&self) -> usize {
        (self.undo.capacity() + self.redo.capacity()) * std::mem::size_of::<Snapshot>()
            + self
                .undo
                .iter()
                .chain(&self.redo)
                .map(Snapshot::bytes)
                .sum::<usize>()
    }
    fn trim_history(&mut self) {
        while self.undo.len() + self.redo.len() > self.limit
            || self.history_bytes() > MAX_HISTORY_BYTES
        {
            let oldest = if !self.undo.is_empty() {
                self.undo.pop_front()
            } else {
                self.redo.pop_front()
            };
            if oldest.is_none() {
                break;
            }
        }
    }
    fn restore(&mut self, state: Snapshot) -> Snapshot {
        let current = Snapshot::capture(&self.area);
        // Keep configuration and kill/yank state; text history owns only text/cursor.
        let yank = self.area.yank_text();
        self.area.cancel_selection();
        self.area.clear();
        self.area.insert_str(&state.text);
        let max = usize::from(u16::MAX);
        self.area.move_cursor(CursorMove::Jump(
            state.cursor.0.min(max) as u16,
            state.cursor.1.min(max) as u16,
        ));
        // A valid 64 KiB draft can put its last row or column one past u16.
        for _ in max..state.cursor.0 {
            self.area.move_cursor(CursorMove::Down);
        }
        for _ in max..state.cursor.1 {
            self.area.move_cursor(CursorMove::Forward);
        }
        self.area.set_yank_text(yank);
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn maximum_draft_cursor_survives_undo_and_redo() {
        for text in ["x".repeat(65_536), "\n".repeat(65_536)] {
            let mut editor = Editor::default();
            editor.insert_str(&text);
            let cursor = editor.cursor();
            editor.input(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
            assert!(editor.undo());
            assert_eq!(editor.cursor(), cursor);
            assert!(editor.redo());
            assert!(editor.undo());
            assert_eq!(editor.cursor(), cursor);
            editor.insert_str("!");
            assert!(editor.lines().join("\n").ends_with('!'));
        }
    }

    #[test]
    fn multiline_history_is_bounded_and_keeps_recent_edits_atomic() {
        let text = "x\n".repeat(32000);
        let mut editor = Editor::default();
        editor.insert_str(&text);
        for _ in 0..128 {
            editor.insert_str("a");
        }
        assert!(editor.history_bytes() <= MAX_HISTORY_BYTES);
        assert!(editor.undo());
        assert_eq!(editor.lines().last(), Some(&"a".repeat(127)));
        assert!(editor.redo());
        assert_eq!(editor.lines().last(), Some(&"a".repeat(128)));
    }

    #[test]
    fn undo_keeps_the_latest_kill_available_for_yank() {
        let mut editor = Editor::default();
        editor.insert_str("keep this kill");
        editor.move_cursor(CursorMove::Head);
        editor.input(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(editor.yank_text(), "keep this kill");
        assert!(editor.undo());
        assert_eq!(editor.lines(), ["keep this kill"]);
        assert_eq!(editor.yank_text(), "keep this kill");
        assert!(editor.redo());
        assert_eq!(editor.yank_text(), "keep this kill");
        editor.input(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(editor.lines(), ["keep this kill"]);
    }
}
