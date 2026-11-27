//! Injected mail envelopes share the transcript's retained source and disclosure.
use super::*;

pub(super) fn envelope(slice: &Slice) -> Option<(&str, &str)> {
    if !matches!(slice.kind, SliceKind::User | SliceKind::Automated) {
        return None;
    }
    let (header, body) = slice.text.split_once('\n')?;
    let sender = header
        .trim_end_matches('\r')
        .strip_prefix("INBOX MESSAGE RECEIVED from ")?;
    let sender = match sender.rsplit_once(" (msg-") {
        Some((name, id))
            if id.ends_with(')')
                && id[..id.len() - 1]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)) =>
        {
            name
        }
        _ => sender,
    };
    (!sender.trim().is_empty()).then_some((sender, body))
}

fn body(source: &str, width: u16, expanded: bool, palette: Palette) -> Vec<Line<'static>> {
    let inset = if width > 2 { "  " } else { "" };
    let available = width.saturating_sub(inset.len() as u16).max(1);
    let mut rows = Vec::new();
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        let wrapped = markdown::wrap(
            vec![Line::styled(markdown::literal(line), palette.normal())],
            available,
        );
        let remaining = if expanded {
            wrapped.len()
        } else {
            2 - rows.len()
        };
        let more = wrapped.len() > remaining || lines.peek().is_some();
        rows.extend(wrapped.into_iter().take(remaining).map(|mut row| {
            row.spans.insert(0, Span::raw(inset));
            row
        }));
        if !expanded && rows.len() == 2 {
            if more {
                if let Some(last) = rows.last_mut() {
                    *last = Line::styled(
                        format!(
                            "{}…",
                            grapt_client::cells::cut(
                                &last.to_string(),
                                usize::from(width.saturating_sub(1))
                            )
                        ),
                        palette.normal().fg(palette.muted),
                    );
                }
            }
            break;
        }
    }
    rows
}

impl Transcript {
    pub(super) fn mail(&mut self, index: usize, width: u16, palette: Palette) -> bool {
        let cell = &self.cells[index];
        let Some((sender, source)) = envelope(&cell.slice) else {
            return false;
        };
        let disclosure = if cell.expanded { "▾" } else { "▸" };
        let mut style = palette.normal().fg(palette.muted);
        if self.tool_focus == Some(disclosure::Focus::Mail(index)) {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        let mut rows = vec![Line::styled(
            tool_groups::fit(
                &format!(
                    "✉ Message received from {} {disclosure}",
                    markdown::literal(sender)
                ),
                width,
            ),
            style,
        )];
        rows.extend(body(source, width, cell.expanded, palette));
        if cell.slice.truncated_bytes > 0 {
            rows.push(Line::styled(
                tool_groups::fit(
                    &format!("[{} source bytes omitted]", cell.slice.truncated_bytes),
                    width,
                ),
                palette.normal().fg(palette.bad),
            ));
        }
        self.paint_epoch = self.paint_epoch.saturating_add(1);
        let height = rows.len();
        let cell = &mut self.cells[index];
        cell.cache = Some(CachedOutput {
            width,
            theme: palette.light,
            rendered: rows,
            row_epochs: vec![self.paint_epoch; height],
            source_end: cell.shown,
            markdown: None,
        });
        cell.height = Some((width, height));
        self.remember_cache(index);
        true
    }
}
