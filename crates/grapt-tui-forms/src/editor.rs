//! The multi-line editor's presentation, shared by the form answer editor and
//! the TUI's composer. It measures whole grapheme clusters, including when the
//! editor's scalar cursor falls within a joined emoji.
use grapt_tui_transcript::{
    markdown::{literal, wrap},
    palette::Palette,
};
use ratatui::{
    style::Modifier,
    text::{Line, Span},
};
use ratatui_textarea::TextArea;
use unicode_segmentation::UnicodeSegmentation;

/// The editor's rows wrapped to `width`, with the cursor's cluster
/// highlighted, and the row that holds the cursor.
pub fn rows(editor: &TextArea<'_>, width: u16, palette: Palette) -> (Vec<Line<'static>>, usize) {
    let ratatui_textarea::DataCursor(row, column) = editor.cursor();
    let normal = palette.normal();
    let cursor = normal.bg(palette.accent).add_modifier(Modifier::BOLD);
    let mut lines = Vec::new();
    for (index, line) in editor.lines().iter().enumerate() {
        if index != row {
            lines.push(Line::styled(literal(line), normal));
            continue;
        }
        let byte = line
            .char_indices()
            .nth(column)
            .map_or(line.len(), |(byte, _)| byte);
        let cluster = line
            .grapheme_indices(true)
            .find(|(start, glyph)| *start <= byte && byte < start + glyph.len());
        let (start, end) = cluster.map_or((line.len(), line.len()), |(start, glyph)| {
            (start, start + glyph.len())
        });
        let glyph = if start == end {
            " ".into()
        } else {
            literal(&line[start..end])
        };
        lines.push(Line::from(vec![
            Span::styled(literal(&line[..start]), normal),
            Span::styled(glyph, cursor),
            Span::styled(literal(&line[end..]), normal),
        ]));
    }
    let rows = wrap(lines, width);
    let cursor_row = rows
        .iter()
        .position(|line| {
            line.spans
                .iter()
                .any(|span| span.style.bg == Some(palette.accent))
        })
        .unwrap_or(0);
    (rows, cursor_row)
}
