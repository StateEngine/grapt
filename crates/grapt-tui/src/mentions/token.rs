// Copyright 2025 OpenAI. Licensed under Apache-2.0.
use crate::editor::Editor as TextArea;
use ratatui_textarea::CursorMove;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Token {
    row: usize,
    start: usize,
    end: usize,
    pub query: String,
}

impl Token {
    pub fn at(editor: &TextArea<'_>) -> Option<Self> {
        let cursor = editor.cursor();
        let line = editor.lines().get(cursor.0)?;
        let byte = line
            .char_indices()
            .nth(cursor.1)
            .map_or(line.len(), |(i, _)| i);
        let start = line[..byte].rfind(char::is_whitespace).map_or(0, |i| {
            i + line[i..].chars().next().map_or(0, char::len_utf8)
        });
        let query = line.get(start..byte)?.strip_prefix('@')?;
        if query.len() > grapt_client::file_finder::MAX_QUERY_BYTES
            || query.chars().any(char::is_control)
        {
            return None;
        }
        let end = line[byte..]
            .find(char::is_whitespace)
            .map_or(line.len(), |i| byte + i);
        Some(Self {
            row: cursor.0,
            start,
            end,
            query: query.to_owned(),
        })
    }

    pub fn insert(&self, editor: &mut TextArea<'static>, path: &str) -> Result<(), String> {
        if Self::at(editor).as_ref() != Some(self) {
            return Err("File query changed; choose a current result".into());
        }
        if path.chars().any(char::is_control) {
            return Err("File path contains controls".into());
        }
        let quoted = if path.chars().any(char::is_whitespace) || path.contains('"') {
            serde_json::to_string(path).map_err(|e| e.to_string())?
        } else {
            path.to_owned()
        };
        let size = editor.lines().iter().map(String::len).sum::<usize>()
            + editor.lines().len().saturating_sub(1);
        if size
            .saturating_sub(self.end - self.start)
            .saturating_add(quoted.len() + 1)
            > crate::MAX_DRAFT
        {
            return Err("Draft exceeds 64 KiB; file reference kept out".into());
        }
        let line = &editor.lines()[self.row];
        let start = line[..self.start].chars().count();
        let end = line[..self.end].chars().count();
        if self.row > u16::MAX as usize || end > u16::MAX as usize {
            return Err("File reference exceeds editor coordinate bounds".into());
        }
        editor.edit(|area| {
            area.cancel_selection();
            area.move_cursor(CursorMove::Jump(self.row as u16, start as u16));
            area.start_selection();
            area.move_cursor(CursorMove::Jump(self.row as u16, end as u16));
            area.insert_str(format!("{quoted} "));
        });
        Ok(())
    }
}
