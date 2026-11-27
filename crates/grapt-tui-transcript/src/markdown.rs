//! Markdown/highlight rows are cached by the viewport, never by domain owners.
//! Paragraph separation.
//! Apache-2.0, Copyright 2025 OpenAI.
use crate::palette::Palette;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::Modifier,
    text::{Line, Span},
};
use std::cell::RefCell;

mod fence;
mod incremental;
mod list;
mod paragraph;
#[cfg(test)]
mod spacing_tests;
mod wrapping;
pub use incremental::Incremental;
pub use wrapping::literal as wrap;

struct Parsed {
    lines: Vec<Line<'static>>,
    stable_source: usize,
    stable_rows: usize,
    global: bool,
}

pub struct Markdown {
    highlighter: RefCell<crate::highlight::Highlighter>,
}

impl Default for Markdown {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Markdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Markdown").finish_non_exhaustive()
    }
}

pub fn literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' => out.push('\n'),
            '\t' => out.push_str("    "),
            ch if ch.is_control()
                || matches!(ch,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}') =>
            {
                use std::fmt::Write;
                let _ = write!(out, "\\u{{{:x}}}", u32::from(ch));
            }
            ch => out.push(ch),
        }
    }
    out
}

impl Markdown {
    /// Diff previews share the existing bounded, asynchronous syntax worker.
    pub(super) fn syntax(
        &self,
        language: &str,
        source: &str,
        palette: Palette,
    ) -> Option<Vec<Line<'static>>> {
        self.highlighter
            .borrow_mut()
            .rows(language, source, palette)
    }
    pub fn new() -> Self {
        Self {
            highlighter: RefCell::new(crate::highlight::Highlighter::default()),
        }
    }

    pub fn set_wake(&self, wake: std::sync::mpsc::SyncSender<()>) {
        self.highlighter.borrow_mut().set_wake(wake);
    }

    pub fn refresh(&self) -> Result<bool, String> {
        self.highlighter.borrow_mut().refresh()
    }

    pub fn new_view(&self) {
        self.highlighter.borrow_mut().new_view();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn render(&self, text: &str, width: u16, palette: Palette) -> Vec<Line<'static>> {
        self.parse_blocks(text, width, palette, true, false).lines
    }

    /// Height metadata skips syntax highlighting and retains no painted rows.
    pub fn height(&self, text: &str, width: u16) -> usize {
        Incremental::height(self, text, width)
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn parse_blocks(
        &self,
        text: &str,
        width: u16,
        palette: Palette,
        highlight: bool,
        leading_gap: bool,
    ) -> Parsed {
        crate::profile::markdown(text.len(), highlight);
        let text = literal(text);
        let mut rows = Vec::new();
        let mut current = Vec::new();
        let mut styles = vec![palette.normal()];
        let mut code: Option<(String, String, bool)> = None;
        let parser = Parser::new_ext(
            &text,
            Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
        );
        let mut global = parser.reference_definitions().iter().next().is_some();
        let (mut depth, mut stable_source, mut stable_row) = (0usize, 0usize, 0usize);
        for (event, range) in parser.into_offset_iter() {
            match &event {
                Event::Start(_) => {
                    if depth == 0 {
                        // Pending output (for example a header-only table)
                        // cannot be assigned to a retained row prefix yet.
                        global |= !current.is_empty();
                        // Content ranges can omit indentation. Restart at the
                        // lexical line boundary so its block syntax survives.
                        stable_source = text[..range.start]
                            .rfind('\n')
                            .map_or(0, |newline| newline + 1);
                        stable_row = rows.len();
                    }
                    depth += 1;
                }
                Event::End(_) => depth = depth.saturating_sub(1),
                Event::Rule if depth == 0 => {
                    global |= !current.is_empty();
                    stable_source = text[..range.start]
                        .rfind('\n')
                        .map_or(0, |newline| newline + 1);
                    stable_row = rows.len();
                }
                Event::Html(_) | Event::InlineHtml(_) => global = true,
                _ => {}
            }
            let style = styles.last().copied().unwrap_or_default();
            match event {
                Event::Start(Tag::CodeBlock(kind)) => {
                    flush(&mut current, &mut rows, width);
                    let complete = !matches!(&kind, CodeBlockKind::Fenced(_))
                        || fence::OpenFence::detect(&text[range.clone()], range.end).is_none();
                    let language = match kind {
                        CodeBlockKind::Fenced(s) => {
                            s.split_whitespace().next().unwrap_or("").to_owned()
                        }
                        CodeBlockKind::Indented => String::new(),
                    };
                    code = Some((language, String::new(), complete));
                }
                Event::End(TagEnd::CodeBlock) => {
                    if let Some((language, source, complete)) = code.take() {
                        self.code(
                            &language,
                            &source,
                            width,
                            palette,
                            &mut rows,
                            highlight && complete,
                        );
                    }
                }
                Event::Text(s) if code.is_some() => {
                    if let Some((_, source, _)) = &mut code {
                        source.push_str(&s);
                    }
                }
                // A new paragraph starts after a
                // pending block separator. Keep that row in the mutable tail,
                // so suffix reparsing and height measurement see the same gap.
                Event::Start(Tag::Paragraph) => {
                    let gap = rows.last().map_or(leading_gap, |row| row.width() > 0);
                    if gap {
                        rows.push(Line::default());
                    }
                }
                Event::Start(Tag::Heading { .. }) => {
                    flush(&mut current, &mut rows, width);
                    styles.push(style.fg(palette.accent).add_modifier(Modifier::BOLD));
                }
                Event::Start(Tag::Strong) => styles.push(style.add_modifier(Modifier::BOLD)),
                Event::Start(Tag::Emphasis) => styles.push(style.add_modifier(Modifier::ITALIC)),
                Event::Start(Tag::Strikethrough) => {
                    styles.push(style.add_modifier(Modifier::CROSSED_OUT))
                }
                Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                    styles.pop();
                }
                Event::End(TagEnd::Heading(_)) => {
                    flush(&mut current, &mut rows, width);
                    styles.pop();
                }
                Event::Start(Tag::Item) => {
                    flush(&mut current, &mut rows, width);
                    current.push(Span::styled("  • ", style.fg(palette.accent)));
                }
                Event::End(TagEnd::Paragraph | TagEnd::Item) => {
                    flush(&mut current, &mut rows, width)
                }
                Event::End(TagEnd::TableRow) => {
                    if !current.is_empty() {
                        rows.extend(wrap(vec![Line::from(std::mem::take(&mut current))], width));
                    }
                }
                Event::End(TagEnd::TableCell) => {
                    current.push(Span::styled(" │ ", style.fg(palette.muted)))
                }
                Event::Text(s) | Event::Html(s) | Event::InlineHtml(s) => {
                    current.push(Span::styled(s.into_string(), style))
                }
                Event::Code(s) => current.push(Span::styled(
                    s.into_string(),
                    style.fg(palette.accent).bg(palette.panel),
                )),
                Event::SoftBreak => current.push(Span::raw(" ")),
                Event::HardBreak => flush(&mut current, &mut rows, width),
                Event::Rule => {
                    flush(&mut current, &mut rows, width);
                    rows.push(Line::styled(
                        "─".repeat(usize::from(width)),
                        style.fg(palette.muted),
                    ));
                }
                Event::TaskListMarker(done) => {
                    current.push(Span::raw(if done { "[x] " } else { "[ ] " }))
                }
                _ => {}
            }
        }
        if let Some((language, source, complete)) = code {
            self.code(
                &language,
                &source,
                width,
                palette,
                &mut rows,
                highlight && complete,
            );
        }
        flush(&mut current, &mut rows, width);
        Parsed {
            lines: rows,
            stable_source,
            stable_rows: stable_row,
            global,
        }
    }

    fn code(
        &self,
        language: &str,
        source: &str,
        width: u16,
        palette: Palette,
        rows: &mut Vec<Line<'static>>,
        highlight: bool,
    ) {
        let mut code_rows = vec![Line::styled(
            format!("  {language}"),
            palette.normal().fg(palette.muted).bg(palette.panel),
        )];
        if !highlight || source.is_empty() {
            code_rows.extend(source.split_inclusive('\n').map(|line| {
                Line::styled(
                    line.trim_end_matches('\n').to_owned(),
                    palette.normal().bg(palette.panel),
                )
            }));
        } else {
            let colored = self
                .highlighter
                .borrow_mut()
                .rows(language, source, palette);
            code_rows.extend(colored.unwrap_or_else(|| crate::highlight::plain(source, palette)));
        }
        rows.extend(wrap(code_rows, width));
    }
}

fn flush(current: &mut Vec<Span<'static>>, rows: &mut Vec<Line<'static>>, width: u16) {
    if !current.is_empty() {
        rows.extend(wrapping::prose(
            vec![Line::from(std::mem::take(current))],
            width,
        ));
    }
}
