//! A full-width quiet user band and leading chevron keep human input distinct.
use crate::{markdown, palette::Palette};
use grapt_client::{cells, SliceKind};
use ratatui::text::{Line, Span};

pub(super) fn is_prompt(kind: SliceKind) -> bool {
    matches!(
        kind,
        SliceKind::User | SliceKind::Automated | SliceKind::SeatStartup
    )
}

fn shade(first: bool, line: Line<'static>, width: u16, p: Palette) -> Line<'static> {
    let width = usize::from(width);
    let inset = match (first, width) {
        (true, 4..) => " › ",
        (false, 4..) => "   ",
        _ => "",
    };
    let padding = width.saturating_sub(cells::cells(inset) + line.width());
    let mut spans = vec![Span::styled(inset, p.normal().fg(p.muted).bg(p.panel))];
    let style = line.style;
    spans.extend(line.spans.into_iter().map(|mut span| {
        span.style = style.patch(span.style).bg(p.panel);
        span
    }));
    spans.push(Span::styled(" ".repeat(padding), p.normal().bg(p.panel)));
    Line::from(spans)
}

pub(super) fn rows(kind: SliceKind, text: &str, width: u16, p: Palette) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::default()];
    }
    let title = match kind {
        SliceKind::Automated => Some("Automated prompt · F8 collapse"),
        SliceKind::SeatStartup => Some("Seat startup prompt · F8 collapse"),
        _ => None,
    };
    let mut body = Vec::new();
    if let Some(title) = title {
        body.push(Line::styled(title.to_owned(), p.normal().fg(p.muted)));
    }
    body.extend(
        text.split('\n')
            .map(|line| Line::styled(markdown::literal(line), p.normal())),
    );
    let body = markdown::wrap(body, if width >= 4 { width - 3 } else { width });
    std::iter::once(Line::default())
        .chain(
            body.into_iter()
                .enumerate()
                .map(|(row, line)| shade(row == 0, line, width, p)),
        )
        .chain(std::iter::once(Line::default()))
        .collect()
}

pub(super) fn notice(title: &str, width: u16, p: Palette) -> Line<'static> {
    let text = cells::cut(
        title,
        usize::from(if width >= 4 { width - 3 } else { width }),
    );
    shade(true, Line::styled(text, p.normal().fg(p.muted)), width, p)
}

#[cfg(test)]
mod tests;
