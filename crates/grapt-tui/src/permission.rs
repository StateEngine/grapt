//! Content-sized approval panels.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Literal previews and authority decisions retain their existing owners.
use grapt_client::{view::permission, Paint, PermissionAsk, Token};
use grapt_tui_transcript::palette::Palette;
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};

fn controls(ask: &PermissionAsk, width: usize, details: bool) -> Vec<String> {
    let mut rows = grapt_client::cells::wrap("[y] Once  [s] Session  [n] Reject", width.max(1));
    if details && ask.allow_always {
        rows.push("[a] Always".into());
    }
    rows
}

fn rows(
    ask: &PermissionAsk,
    notice: Option<&str>,
    width: usize,
    details: bool,
) -> Vec<(String, grapt_client::Style)> {
    if details {
        permission::review_rows_with_notice(ask, notice, width)
    } else {
        permission::compact_rows(ask, notice, width)
    }
}

/// Reserve only the content's height, with a bounded expanded review window.
pub fn height(ask: &PermissionAsk, notice: Option<&str>, width: u16, details: bool) -> u16 {
    let width = usize::from(width.saturating_sub(2).max(1));
    (rows(ask, notice, width, details).len() + controls(ask, width, details).len() + 2)
        .clamp(6, if details { 18 } else { 10 }) as u16
}

/// Paint above the composer, keeping decision controls fixed while paging.
pub fn draw(
    frame: &mut Frame<'_>,
    panel: Rect,
    ask: &PermissionAsk,
    notice: Option<&str>,
    scroll: &mut usize,
    details: bool,
    palette: Palette,
) -> usize {
    let style = palette.normal().bg(palette.panel);
    let block = Block::default()
        .borders(Borders::LEFT)
        .border_style(style.fg(palette.accent))
        .style(style);
    let mut inner = block.inner(panel);
    inner.x = inner.x.saturating_add(u16::from(inner.width > 0));
    inner.width = inner.width.saturating_sub(1);
    frame.render_widget(block, panel);
    let title = if permission::source_truncated(ask) {
        "Permission · source preview truncated · F4 details"
    } else if details {
        "Permission · F4 collapse · PgUp/PgDn"
    } else {
        "Permission · F4 details"
    };
    frame.render_widget(
        Paragraph::new(title).style(style.fg(palette.accent)),
        Rect {
            height: inner.height.min(1),
            ..inner
        },
    );
    let choices = controls(ask, usize::from(inner.width), details);
    let footer_height = (choices.len() as u16).min(inner.height.saturating_sub(1));
    let content = Rect {
        y: inner.y.saturating_add(u16::from(inner.height > 0)),
        height: inner.height.saturating_sub(footer_height + 2),
        ..inner
    };
    let rows = rows(ask, notice, usize::from(inner.width.max(1)), details);
    *scroll = (*scroll).min(rows.len().saturating_sub(usize::from(content.height)));
    let end = scroll
        .saturating_add(usize::from(content.height))
        .min(rows.len());
    let visible = rows[*scroll..end]
        .iter()
        .map(|(text, source)| {
            let color = match source.fg {
                Paint::Token(Token::Accent) => palette.accent,
                Paint::Token(Token::DiffAdd) => palette.good,
                Paint::Token(Token::DiffDel) => palette.bad,
                Paint::Token(Token::Muted) => palette.muted,
                _ => palette.text,
            };
            Line::styled(text.clone(), style.fg(color))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(visible), content);
    frame.render_widget(
        Paragraph::new(choices.into_iter().map(Line::raw).collect::<Vec<_>>())
            .style(style.fg(palette.accent)),
        Rect {
            y: inner.bottom().saturating_sub(footer_height),
            height: footer_height,
            ..inner
        },
    );
    usize::from(content.height).saturating_sub(1).max(1)
}

#[cfg(test)]
mod tests;
