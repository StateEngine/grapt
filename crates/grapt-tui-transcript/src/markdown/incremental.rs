//! Apache-2.0, Copyright OpenAI.
//! Retain complete top-level blocks; only the last block can change on append.
use super::{Markdown, Palette};
use ratatui::text::Line;

#[derive(Debug, Default)]
pub struct Incremental {
    stable_source: usize,
    stable_rows: usize,
    global: bool,
    normalized: bool,
    checked: usize,
    open_fence: Option<super::fence::OpenFence>,
    open_list: Option<super::list::OpenList>,
    rendered_rows: usize,
    open_paragraph: Option<super::paragraph::OpenParagraph>,
}

impl Incremental {
    /// The caller owns width/theme/replacement invalidation and the bounded row
    /// cache. Rows include its one heading; trailing decoration is discarded.
    /// Returns the unchanged row prefix for the caller’s damage tracking.
    pub fn append(
        &mut self,
        renderer: &Markdown,
        source: &str,
        width: u16,
        palette: Palette,
        rows: &mut Vec<Line<'static>>,
    ) -> usize {
        self.append_mode(renderer, source, width, palette, rows, true)
    }

    pub(super) fn height(renderer: &Markdown, source: &str, width: u16) -> usize {
        let mut state = Self::default();
        let mut rows = vec![Line::default()];
        state.append_mode(
            renderer,
            source,
            width,
            Palette::new(false),
            &mut rows,
            false,
        );
        rows.len().saturating_sub(1)
    }

    fn append_mode(
        &mut self,
        renderer: &Markdown,
        source: &str,
        width: u16,
        palette: Palette,
        rows: &mut Vec<Line<'static>>,
        highlight: bool,
    ) -> usize {
        if self.checked == 0 {
            if let Some((start, end)) = super::list::cold_run(source) {
                let mut offset = start;
                for (index, line) in source[start..end].split_inclusive('\n').enumerate() {
                    offset += line.len();
                    if (index + 1) % 16 != 0 && offset != end {
                        continue;
                    }
                    self.append_inner(renderer, &source[..offset], width, palette, rows, highlight);
                    // A lexical list inside an open fence or another container
                    // cannot be seeded as a top-level list.
                    if self.open_list.is_none() {
                        *self = Self::default();
                        rows.truncate(1);
                        return self
                            .append_inner(renderer, source, width, palette, rows, highlight);
                    }
                }
                if end < source.len() {
                    self.append_inner(renderer, source, width, palette, rows, highlight);
                }
                return 1;
            }
        }
        self.append_inner(renderer, source, width, palette, rows, highlight)
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn append_inner(
        &mut self,
        renderer: &Markdown,
        source: &str,
        width: u16,
        palette: Palette,
        rows: &mut Vec<Line<'static>>,
        highlight: bool,
    ) -> usize {
        // Literal escaping changes byte offsets. Keep that uncommon input on
        // the full parser path rather than retaining an incorrect boundary.
        self.normalized |= source[self.checked..].chars().any(|ch| {
            (ch.is_control() && ch != '\n')
                || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        });
        self.checked = source.len();
        self.global |= self.normalized;
        if self.global {
            self.stable_source = 0;
            self.stable_rows = 0;
        }
        // A proven literal paragraph can only grow by soft breaks and text.
        // Reference/HTML fallback remains armed if later syntax invalidates it.
        if !self.normalized {
            if let Some(mut paragraph) = self.open_paragraph.take() {
                if let Some(delta) = paragraph.append(source) {
                    let retained = self.rendered_rows.saturating_sub(1);
                    if let Some(last) = rows.get(retained) {
                        let mut tail = last.clone();
                        for line in delta.split_terminator('\n') {
                            tail.spans.push(ratatui::text::Span::raw(" "));
                            tail.spans.push(ratatui::text::Span::styled(
                                line.to_owned(),
                                palette.normal(),
                            ));
                        }
                        rows.truncate(retained);
                        rows.extend(super::wrapping::prose(vec![tail], width));
                        self.rendered_rows = rows.len();
                        self.open_paragraph = Some(paragraph);
                        return retained;
                    }
                }
            }
        }
        if !self.global {
            if let Some(mut list) = self.open_list.take() {
                if list.pending_blank(source) {
                    rows.truncate(self.rendered_rows);
                    self.open_list = Some(list);
                    return self.rendered_rows;
                }
                if let Some(start) = list.following_paragraph(source) {
                    self.stable_source = start;
                    self.stable_rows = self.rendered_rows.saturating_sub(1);
                }
                if let Some(delta) = list.append(source) {
                    let parsed = renderer.parse_blocks(delta, width, palette, highlight, false);
                    if !parsed.global {
                        let retained = self.rendered_rows;
                        rows.truncate(retained);
                        rows.extend(parsed.lines);
                        self.rendered_rows = rows.len();
                        self.open_list = Some(list);
                        return retained;
                    }
                }
            }
            if let Some(mut fence) = self.open_fence.take() {
                if let Some(delta) = fence.append(source) {
                    let retained = self.rendered_rows;
                    rows.truncate(retained);
                    rows.extend(super::wrap(crate::highlight::plain(delta, palette), width));
                    self.rendered_rows = rows.len();
                    self.open_fence = Some(fence);
                    return retained;
                }
            }
        }
        // The unstable tail is being replaced. Release it before parsing its
        // replacement so a long open block is not retained twice at peak.
        rows.truncate(1 + self.stable_rows);
        let leading_gap = self.stable_rows > 0 && rows.last().is_some_and(|row| row.width() > 0);
        let mut parsed = renderer.parse_blocks(
            &source[self.stable_source..],
            width,
            palette,
            highlight,
            leading_gap,
        );
        if parsed.global {
            self.global = true;
            if self.stable_source != 0 {
                self.stable_source = 0;
                self.stable_rows = 0;
                rows.truncate(1);
                parsed = renderer.parse_blocks(source, width, palette, highlight, false);
            }
        }
        let prefix = 1 + self.stable_rows;
        rows.truncate(prefix);
        self.stable_source += parsed.stable_source;
        self.stable_rows += parsed.stable_rows;
        rows.extend(parsed.lines);
        self.rendered_rows = rows.len();
        self.open_list = (!self.global)
            .then(|| super::list::OpenList::detect(&source[self.stable_source..], source.len()))
            .flatten();
        self.open_paragraph = (!self.normalized)
            .then(|| {
                super::paragraph::OpenParagraph::detect(&source[self.stable_source..], source.len())
            })
            .flatten();
        self.open_fence = (!self.global)
            .then(|| super::fence::OpenFence::detect(&source[self.stable_source..], source.len()))
            .flatten();
        prefix
    }
}

#[cfg(test)]
mod tests;
