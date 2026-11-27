//! Retained diff source, per-file folds and width-independent layout bounds.
//! Diff gutters and theme fills for the retained transcript.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
use super::*;
use ratatui::style::Color;
use std::ops::Range;

mod contrast;

const PREVIEW: usize = 10;
const FILES: usize = 64;
const EXPANDED: usize = 512;

pub(super) struct Diff {
    lines: Vec<(Range<usize>, Mark)>,
    files: Vec<File>,
    truncated: usize,
}
struct File {
    key: String,
    header: usize,
    body: Range<usize>,
    expanded: bool,
}

impl Diff {
    pub(super) fn new(slice: &Slice, previous: Option<&Self>) -> Option<Self> {
        if slice.kind != SliceKind::Tool || !slice.marks.contains(&Mark::Header) {
            return None;
        }
        let mut offset = 0;
        let lines: Vec<_> = slice
            .text
            .split('\n')
            .zip(&slice.marks)
            .map(|(text, mark)| {
                let start = offset;
                offset += text.len() + 1;
                (start..start + text.len(), *mark)
            })
            .collect();
        let headers: Vec<_> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, (_, mark))| (*mark == Mark::Header).then_some(i))
            .take(FILES)
            .collect();
        let files = headers
            .iter()
            .enumerate()
            .map(|(i, &header)| {
                let heading = &slice.text[lines[header].0.clone()];
                let key = heading
                    .rsplit_once(" · Added ")
                    .map_or(heading, |(path, _)| path)
                    .to_owned();
                let expanded = previous
                    .and_then(|old| old.files.iter().find(|f| f.key == key))
                    .is_some_and(|file| file.expanded);
                File {
                    key,
                    header,
                    body: header + 1..headers.get(i + 1).copied().unwrap_or(lines.len()),
                    expanded,
                }
            })
            .collect();
        Some(Self {
            lines,
            files,
            truncated: slice.truncated_bytes,
        })
    }

    fn count(file: &File) -> usize {
        file.body
            .len()
            .min(if file.expanded { EXPANDED } else { PREVIEW })
    }
    fn fold(file: &File) -> bool {
        file.body.len() > PREVIEW
    }

    pub(super) fn height(&self) -> usize {
        self.files
            .iter()
            .map(|f| 2 + Self::count(f) + usize::from(Self::fold(f)))
            .sum::<usize>()
            + self.files.len().saturating_sub(1)
            + 1
            + usize::from(self.truncated > 0)
    }

    pub(super) fn target(&self, row: usize) -> Option<usize> {
        let mut start = 0;
        for (i, file) in self.files.iter().enumerate() {
            let fold = start + 2 + Self::count(file);
            if (row == start || row == start + 1 || row == fold) && Self::fold(file) {
                return Some(i);
            }
            start = fold + usize::from(Self::fold(file)) + usize::from(i + 1 < self.files.len());
        }
        None
    }

    pub(super) fn expanded(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| f.expanded)
            .map(|f| f.key.clone())
            .collect()
    }
    pub(super) fn restore(&mut self, files: &[String]) {
        for file in &mut self.files {
            file.expanded = files.contains(&file.key);
        }
    }
    pub(super) fn toggle(&mut self, file: usize) {
        if let Some(file) = self.files.get_mut(file) {
            file.expanded = !file.expanded;
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn rows(
        &self,
        source: &str,
        width: u16,
        palette: Palette,
        focused: Option<usize>,
        renderer: &Markdown,
    ) -> Vec<Line<'static>> {
        let mut rows = Vec::with_capacity(self.height());
        for (i, file) in self.files.iter().enumerate() {
            let heading = &source[self.lines[file.header].0.clone()];
            let (path, counts) = heading
                .rsplit_once(" · Added ")
                .map_or((heading, ""), |(path, counts)| (path, counts));
            let path = ["apply_patch ", "patch ", "edit ", "write "]
                .into_iter()
                .find_map(|prefix| path.strip_prefix(prefix))
                .unwrap_or(path);
            let mut style = palette
                .normal()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD);
            if focused == Some(i) {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            rows.push(Line::styled(
                tool_groups::fit(&format!("Update({path})"), width),
                style,
            ));
            rows.push(Line::styled(
                tool_groups::fit(
                    &if counts.is_empty() {
                        "Line counts unavailable".into()
                    } else {
                        format!("Added {counts}")
                    },
                    width,
                ),
                palette.normal().fg(palette.muted),
            ));
            let language = path.rsplit('.').next().unwrap_or("");
            let shown: Vec<_> = self.lines[file.body.clone()]
                .iter()
                .take(Self::count(file))
                .map(|(range, _)| tool_groups::fit(&source[range.clone()], width))
                .collect();
            let code = shown.join("\n");
            let syntax = if shown.len() <= 128 && code.len() <= 8192 && language.len() <= 12 {
                renderer.syntax(language, &code, palette)
            } else {
                None
            };
            for (row, (range, mark)) in self.lines[file.body.clone()]
                .iter()
                .take(Self::count(file))
                .enumerate()
            {
                let (background, foreground) = match (mark, palette.light) {
                    (Mark::Insert, true) => (Color::Rgb(185, 238, 201), palette.text),
                    (Mark::Insert, false) => (Color::Rgb(28, 78, 49), palette.text),
                    (Mark::Delete, true) => (Color::Rgb(255, 210, 210), palette.text),
                    (Mark::Delete, false) => (Color::Rgb(100, 40, 40), palette.text),
                    (Mark::Gap | Mark::Plain, _) => (palette.background, palette.muted),
                    _ => (palette.background, palette.text),
                };
                // Clip instead of wrapping: a single minified line must never
                // create thousands of frame rows or invalidate fold hit geometry.
                let mut line = syntax
                    .as_ref()
                    .and_then(|lines| lines.get(row))
                    .cloned()
                    .filter(|_| matches!(mark, Mark::Insert | Mark::Delete | Mark::Context))
                    .unwrap_or_else(|| Line::from(tool_groups::fit(&source[range.clone()], width)));
                line.style = palette.normal().fg(foreground).bg(background);
                for span in &mut line.spans {
                    span.style = span
                        .style
                        .fg(contrast::foreground(
                            span.style.fg.unwrap_or(foreground),
                            background,
                        ))
                        .bg(background)
                        .remove_modifier(Modifier::DIM);
                }
                rows.push(line);
            }
            if Self::fold(file) {
                let text = if file.expanded {
                    if file.body.len() > EXPANDED {
                        format!(
                            "… {} lines beyond display limit · click or Ctrl+O to collapse",
                            file.body.len() - EXPANDED
                        )
                    } else {
                        "▴ click or Ctrl+O to collapse".into()
                    }
                } else {
                    format!(
                        "… {} more lines · click or Ctrl+O to expand",
                        file.body.len() - PREVIEW
                    )
                };
                rows.push(Line::styled(
                    tool_groups::fit(&text, width),
                    palette.normal().fg(palette.accent),
                ));
            }
            // A blank row between back-to-back file blocks; the block after
            // the last file keeps only the trailing row of the cell.
            if i + 1 < self.files.len() {
                rows.push(Line::default());
            }
        }
        if self.truncated > 0 {
            rows.push(Line::styled(
                format!("[{} source bytes omitted]", self.truncated),
                palette.normal().fg(palette.bad),
            ));
        }
        rows.push(Line::default());
        rows
    }
}

impl Transcript {
    pub(super) fn inline_diff(&mut self, index: usize, width: u16, palette: Palette) -> bool {
        let Some(diff) = &self.cells[index].diff else {
            return false;
        };
        let focused = match self.tool_focus {
            Some(disclosure::Focus::Diff(cell, file)) if cell == index => Some(file),
            _ => None,
        };
        let rendered = diff.rows(
            &self.cells[index].slice.text,
            width,
            palette,
            focused,
            &self.renderer,
        );
        self.paint_epoch = self.paint_epoch.saturating_add(1);
        let height = rendered.len();
        let cell = &mut self.cells[index];
        cell.cache = Some(CachedOutput {
            width,
            theme: palette.light,
            rendered,
            row_epochs: vec![self.paint_epoch; height],
            source_end: cell.shown,
            markdown: None,
        });
        cell.height = Some((width, height));
        self.remember_cache(index);
        true
    }

    pub(super) fn toggle_diff(&mut self, index: usize, file: usize) {
        self.pin_disclosure();
        if let Some(cell) = self.cells.get_mut(index) {
            if let Some(diff) = &mut cell.diff {
                diff.toggle(file);
                cell.cache = None;
                cell.height = None;
                self.revision = self.revision.saturating_add(1);
            }
        }
    }
}
