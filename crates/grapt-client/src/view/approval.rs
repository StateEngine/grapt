//! Semantic patch roles shared by approval review rows.
use crate::style::{Style, Token};

// Retained from SC-TUI-05.
// Apache-2.0, Copyright 2025 OpenAI (licenses/openai-codex/).
pub(super) fn patch_style(line: &str) -> Style {
    if line.starts_with("+++ ")
        || line.starts_with("--- ")
        || line.starts_with("@@")
        || line.starts_with("***")
    {
        Style::token(Token::Accent)
    } else if line.starts_with('+') {
        Style::token(Token::DiffAdd)
    } else if line.starts_with('-') {
        Style::token(Token::DiffDel)
    } else if line.starts_with('\\') {
        Style::token(Token::Muted)
    } else {
        Style::PLAIN
    }
}

#[cfg(test)]
mod tests;
