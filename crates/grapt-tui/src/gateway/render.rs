use super::*;
use ratatui::{
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

pub(super) fn literal(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch.is_control() {
                ch.escape_default().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .take(4096)
        .collect()
}
pub(super) fn draw(gateway: &Gateway, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
    let title = match gateway.screen {
        Screen::Projects => "Projects",
        Screen::Agents => "StateEngine agents",
        Screen::Sessions => "Active sessions",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" StateEngine · {title} · fixture preview "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let header = ratatui::layout::Rect {
        height: inner.height.min(2),
        ..inner
    };
    let path = match (&gateway.project, &gateway.agent) {
        (Some(p), Some(a)) => format!("{} / {}", literal(&p.name), literal(&a.name)),
        (Some(p), _) => literal(&p.name),
        _ => String::new(),
    };
    frame.render_widget(
        Paragraph::new(format!("{path}\nSearch: {}", literal(&gateway.query))),
        header,
    );
    let list = ratatui::layout::Rect {
        y: header.bottom(),
        height: inner.height.saturating_sub(4),
        ..inner
    };
    let rows = gateway.labels();
    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(if gateway.pending.is_some() {
                "Loading…"
            } else if gateway.complete {
                "No matching entries on this page"
            } else {
                "No entries loaded"
            }),
            list,
        );
    } else {
        let mut state = ListState::default().with_selected(Some(gateway.selected));
        frame.render_stateful_widget(
            List::new(
                rows.into_iter()
                    .map(|(_, label)| ListItem::new(Line::raw(label))),
            )
            .highlight_symbol("› "),
            list,
            &mut state,
        );
    }
    frame.render_widget(
        Paragraph::new(format!(
            "{}\n↑↓ choose · Enter continue · Esc back · F5 reload",
            literal(&gateway.status)
        )),
        ratatui::layout::Rect {
            y: inner.bottom().saturating_sub(2),
            height: inner.height.min(2),
            ..inner
        },
    );
}
