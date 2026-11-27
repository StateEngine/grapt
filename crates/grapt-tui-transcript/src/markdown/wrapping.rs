//! Styled source ranges and grapheme-safe breaks.
//! Copyright 2025 OpenAI. Apache-2.0. Modified by Grapt: keep the existing
//! literal wrapper and use whitespace opportunities only for Markdown prose.
use ratatui::{
    style::Style,
    text::{Line, Span},
};
use std::{borrow::Cow, ops::Range};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn literal(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    wrap(lines, width, false)
}

pub(super) fn prose(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    wrap(lines, width, true)
}

fn wrap(lines: Vec<Line<'static>>, width: u16, words: bool) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut out = Vec::new();
    for line in lines {
        // A style boundary may divide a ZWJ cluster or an ordinary word.
        // Choose breaks from the whole line, then restore its style runs.
        let mut text = String::new();
        let mut styles = Vec::new();
        for span in line.spans {
            text.push_str(&span.content);
            if !span.content.is_empty() {
                styles.push((text.len(), line.style.patch(span.style)));
            }
        }
        let literal_range = 0..text.len();
        let ranges: Cow<'_, [Range<usize>]> = if words {
            Cow::Owned(prose_ranges(&text, width))
        } else {
            Cow::Borrowed(std::slice::from_ref(&literal_range))
        };
        let mut style_index = 0;
        for range in ranges.iter() {
            let mut row = Vec::new();
            let mut used = 0;
            let mut run = String::new();
            let mut run_style = Style::default();
            for (relative, glyph) in text[range.clone()].grapheme_indices(true) {
                let offset = range.start + relative;
                while style_index + 1 < styles.len() && offset >= styles[style_index].0 {
                    style_index += 1;
                }
                let style = styles
                    .get(style_index)
                    .map_or(line.style, |(_, style)| *style);
                let cost = glyph.width();
                if used + cost > width && used > 0 {
                    if !run.is_empty() {
                        row.push(Span::styled(std::mem::take(&mut run), run_style));
                    }
                    out.push(Line::from(std::mem::take(&mut row)));
                    used = 0;
                }
                if style != run_style && !run.is_empty() {
                    row.push(Span::styled(std::mem::take(&mut run), run_style));
                }
                run_style = style;
                if cost > width {
                    run.push('�');
                    used += 1;
                } else {
                    run.push_str(glyph);
                    used += cost;
                }
            }
            if !run.is_empty() {
                row.push(Span::styled(run, run_style));
            }
            out.push(Line::from(row));
        }
    }
    out
}

fn break_space(glyph: &str) -> bool {
    glyph
        .chars()
        .all(|ch| ch.is_whitespace() && !matches!(ch, '\u{a0}' | '\u{202f}'))
}

/// Keep an ordinary word together whenever it fits on a fresh row. Oversized
/// tokens fall back to the same whole-grapheme boundaries as literal output.
fn prose_ranges(text: &str, width: usize) -> Vec<Range<usize>> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0;
    // Start/end of the latest whitespace run and width through its end.
    let mut space: Option<(usize, usize, usize)> = None;
    for (offset, glyph) in text.grapheme_indices(true) {
        let end = offset + glyph.len();
        let cost = if glyph.width() > width {
            1
        } else {
            glyph.width()
        };
        if break_space(glyph) {
            let first = space
                .filter(|(_, end, _)| *end == offset)
                .map_or(offset, |(first, _, _)| first);
            used += cost;
            space = Some((first, end, used));
            continue;
        }
        if used + cost > width && used > 0 {
            if let Some((first, end, through)) = space.filter(|(first, _, _)| *first > start) {
                rows.push(start..first);
                start = end;
                used -= through;
            } else {
                rows.push(start..offset);
                start = offset;
                used = 0;
            }
            space = None;
        }
        used += cost;
    }
    let end = space
        .filter(|(_, end, _)| *end == text.len())
        .map_or(text.len(), |(first, _, _)| first);
    rows.push(start..end);
    rows
}

#[cfg(test)]
mod tests;
