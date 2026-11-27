//! Ordinary product notices follow SessionNoticeMessageV2's subdued inline row.
use crate::{markdown, palette::Palette};
use grapt_client::cells;
use ratatui::text::{Line, Span};

pub(super) fn rows(text: &str, width: u16, p: Palette) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::default()];
    }
    let prefix = if width >= 6 {
        "   ◈ "
    } else if width >= 3 {
        "◈ "
    } else {
        ""
    };
    let inset = cells::cells(prefix) as u16;
    let body = markdown::wrap(
        text.split('\n')
            .map(|line| Line::raw(markdown::literal(line)))
            .collect(),
        width.saturating_sub(inset),
    );
    body.into_iter()
        .enumerate()
        .map(|(index, line)| {
            let lead = if index == 0 {
                prefix.to_owned()
            } else {
                " ".repeat(usize::from(inset))
            };
            Line::from(vec![
                Span::styled(lead, p.normal().fg(p.muted)),
                Span::styled(line.to_string(), p.normal().fg(p.muted)),
            ])
        })
        .collect()
}
