//! File changes a tool asked for, shown as a diff (SC-TUI-04).
//!
//! Copyright 2025 OpenAI, Apache-2.0
//! (license and NOTICE in `licenses/openai-codex/`). Modified by Grapt:
//! the change comes from a tool's own input rather than a unified diff, lines
//! are compared here instead of by `diffy`, paths are shown as the tool gave
//! them instead of against a git root, and colour is left to the view, which
//! reads each row's [`Mark`].
//!
//! The row text stays plain: the view styles a row by its mark, so a snapshot
//! of the transcript reads the same characters a terminal shows.

use crate::wire::{DurableTool, DurableToolState};

/// What one logical row of a diff presentation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// The tool's own row, or any row that is not part of the diff.
    Plain,
    /// A summary or file row: a verb, a path and its line counts.
    Header,
    Insert,
    Delete,
    Context,
    /// Unchanged lines left out between two shown stretches.
    Gap,
}

/// Rows a single file shows before the rest is counted instead.
const MAX_FILE_ROWS: usize = 256;

mod execution;
mod patch;
mod render;
/// Unchanged lines kept on each side of a change.
const CONTEXT: usize = 3;
/// Lines compared one by one; larger edits show as removed then added.
const MAX_COMPARED_CELLS: usize = 40_000;

/// The text and row marks for a `patch`, `write` or `edit` call, or `None`
/// for any other tool and for input that is not (yet) a whole object.
pub(super) fn present(tool: &DurableTool, first: &str) -> Option<(String, Vec<Mark>)> {
    let (input, output, error) = match &tool.state {
        DurableToolState::Streaming { .. } => return None,
        DurableToolState::Running { input } => (input, None, None),
        DurableToolState::Completed { input, output } => (input, Some(output.as_str()), None),
        DurableToolState::Error { input, error, .. } => (input, None, Some(error)),
    };
    let input: serde_json::Value = serde_json::from_str(input).ok()?;
    let recorded = if matches!(
        tool.name.as_str(),
        "patch" | "apply_patch" | "edit" | "write"
    ) {
        output.and_then(execution::changes)
    } else {
        None
    };
    let has_receipt = recorded.is_some();
    let mut changes = if let Some(changes) = recorded {
        changes
    } else {
        match tool.name.as_str() {
            "apply_patch" => patch::parse(input.get("input")?.as_str()?)?,
            "patch" => input
                .get("operations")?
                .as_array()?
                .iter()
                .map(operation)
                .collect::<Option<Vec<_>>>()?,
            "write" => {
                let path = shown(input.get("path")?.as_str()?);
                if let Some((first, old, new)) = output.and_then(receipt) {
                    vec![compare(path, &old, &new, Some(first))]
                } else {
                    let mut change = added(Action::Write, path, input.get("content")?.as_str()?);
                    if output
                        .and_then(|o| serde_json::from_str::<serde_json::Value>(o).ok())
                        .is_some_and(|o| {
                            o.get("overwritten") == Some(&serde_json::Value::Bool(true))
                        })
                    {
                        let receipt: serde_json::Value =
                            output.and_then(|o| serde_json::from_str(o).ok())?;
                        let counts = receipt.get("files").and_then(|f| f.get(0));
                        match counts.and_then(|f| {
                            Some((f.get("additions")?.as_u64()?, f.get("deletions")?.as_u64()?))
                        }) {
                            Some((added, removed)) => {
                                change.added = added as usize;
                                change.removed = removed as usize;
                            }
                            None => change.counts_known = false,
                        }
                        change.rows.shown.insert(0, Row::Gap);
                        change
                            .path
                            .push_str(" (replacement content; original diff unavailable)");
                    }
                    if !output
                        .and_then(|o| serde_json::from_str::<serde_json::Value>(o).ok())
                        .is_some_and(|o| {
                            o.get("created") == Some(&serde_json::Value::Bool(true))
                                || o.get("overwritten") == Some(&serde_json::Value::Bool(true))
                        })
                    {
                        change.counts_known = false;
                        change
                            .path
                            .push_str(" (requested content; preimage unavailable)");
                    }
                    vec![change]
                }
            }
            "edit" => vec![edit(&input, output)?],
            _ => return None,
        }
    };
    if matches!(tool.name.as_str(), "apply_patch" | "patch")
        && !has_receipt
        && matches!(tool.state, DurableToolState::Completed { .. })
    {
        for change in &mut changes {
            change.counts_known = false;
            change
                .path
                .push_str(" (applied diff unavailable; requested change)");
        }
    }
    if changes.is_empty() {
        return None;
    }
    let mut out = Out::default();
    out.push(first.to_owned(), Mark::Plain);
    render::render(&changes, &tool.name, &tool.state, &mut out);
    if let Some(error) = error {
        out.push(
            format!("  error {}: {}", shown(&error.kind), shown(&error.message)),
            Mark::Plain,
        );
    }
    Some((out.text, out.marks))
}

#[derive(Clone, Copy)]
enum Action {
    Write,
    Add,
    Edit,
    Delete,
    Rename,
}

/// One requested file change and its preview rows.
struct Change {
    action: Action,
    path: String,
    added: usize,
    removed: usize,
    counts_known: bool,
    rows: Rows,
    /// Whether the rows are whole lines of the file, so that a last line
    /// without a break ends the file rather than only the tool's text.
    whole_lines: bool,
}

impl Change {
    fn bare(action: Action, path: String) -> Self {
        Self {
            action,
            path,
            added: 0,
            removed: 0,
            counts_known: !matches!(action, Action::Delete | Action::Rename),
            rows: Rows::default(),
            whole_lines: true,
        }
    }
}

enum Row {
    /// A line, with its number in the file when the tool's input says it.
    /// `ended` is false for a last line without a line break.
    Line {
        old: Option<usize>,
        new: Option<usize>,
        mark: Mark,
        text: String,
        ended: bool,
    },
    Gap,
}

/// The first rows of a file and a count of the rest, which are never built.
#[derive(Default)]
struct Rows {
    shown: Vec<Row>,
    hidden: usize,
}

impl Rows {
    fn push(&mut self, row: impl FnOnce() -> Row) {
        if self.shown.len() < MAX_FILE_ROWS {
            self.shown.push(row());
        } else {
            self.hidden += 1;
        }
    }
}

fn operation(op: &serde_json::Value) -> Option<Change> {
    let path = |key| op.get(key).and_then(serde_json::Value::as_str).map(shown);
    Some(match op.get("op")?.as_str()? {
        "create" => added(Action::Add, path("path")?, op.get("content")?.as_str()?),
        "update" => compare(
            path("path")?,
            op.get("old_text")?.as_str()?,
            op.get("new_text")?.as_str()?,
            None,
        ),
        "delete" => Change::bare(Action::Delete, path("path")?),
        "rename" => Change::bare(
            Action::Rename,
            format!("{} → {}", path("from")?, path("to")?),
        ),
        _ => return None,
    })
}

/// Lines with their breaks, so a change of line ending, or of the break
/// after the last line, is a change.
fn split(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// A line as shown: its `\n` is the row break, and the `\r` of a CRLF
/// break is the same break, so it is dropped from an ended line; a lone
/// trailing `\r` on a line without a break is a content byte and stays
/// spelled out, as do other control characters.
fn line(number: Option<usize>, mark: Mark, raw: &str) -> Row {
    let ended = raw.ends_with('\n');
    let text = raw.strip_suffix('\n').unwrap_or(raw);
    let text = if ended {
        text.strip_suffix('\r').unwrap_or(text)
    } else {
        text
    };
    Row::Line {
        old: if mark == Mark::Insert { None } else { number },
        new: if mark == Mark::Delete { None } else { number },
        mark,
        text: shown(text),
        ended,
    }
}

/// A whole new file: every line is added and its number is real.
fn added(action: Action, path: String, content: &str) -> Change {
    let mut change = Change::bare(action, path);
    for (index, raw) in split(content).into_iter().enumerate() {
        change.added += 1;
        change
            .rows
            .push(|| line(Some(index + 1), Mark::Insert, raw));
    }
    change
}

/// An `edit` call. Once it has run, its receipt holds the whole lines it
/// changed and where they start, so the rows are numbered; before that, or
/// when the receipt left its patch out, the call's own texts are compared
/// without numbers.
fn edit(input: &serde_json::Value, output: Option<&str>) -> Option<Change> {
    let path = shown(input.get("path")?.as_str()?);
    if let Some((first, old, new)) = output.and_then(receipt) {
        return Some(compare(path, &old, &new, Some(first)));
    }
    let mut change = compare(
        format!("{path} (requested change; applied diff unavailable)"),
        input.get("old_text")?.as_str()?,
        input.get("new_text")?.as_str()?,
        None,
    );
    change.counts_known = false;
    Some(change)
}

/// The first line, removed lines and added lines of an edit receipt's one
/// hunk, or `None` when the receipt is not one whole such patch: its header,
/// its declared counts and its lines must all agree, and every line must be
/// numberable.
fn receipt(output: &str) -> Option<(usize, String, String)> {
    let receipt: serde_json::Value = serde_json::from_str(output).ok()?;
    let [file] = receipt.get("files")?.as_array()?.as_slice() else {
        return None;
    };
    if file.get("patchOmitted")? != false {
        return None;
    }
    let mut lines = file
        .get("patch")?
        .as_str()?
        .split_inclusive('\n')
        .peekable();
    let path = lines.next()?.strip_prefix("--- ")?;
    if lines.next()?.strip_prefix("+++ ")? != path {
        return None;
    }
    let (first, removed, added) = hunk(lines.next()?)?;
    first.checked_add(removed.max(added))?;
    let old = side(&mut lines, '-', removed)?;
    let new = side(&mut lines, '+', added)?;
    lines.next().is_none().then_some((first, old, new))
}

/// `@@ -N,d +N,a @@`: where the edit starts, and how many lines it removes
/// and adds. The edit tool starts both sides on the same line.
fn hunk(header: &str) -> Option<(usize, usize, usize)> {
    let (old, new) = header
        .strip_prefix("@@ -")?
        .strip_suffix(" @@\n")?
        .split_once(" +")?;
    let range = |range: &str| -> Option<(usize, usize)> {
        let (start, count) = range.split_once(',')?;
        Some((number(start)?, number(count)?))
    };
    let ((first, removed), (start, added)) = (range(old)?, range(new)?);
    (first >= 1 && start == first).then_some((first, removed, added))
}

/// A count written only in decimal digits.
fn number(text: &str) -> Option<usize> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// `count` whole lines marked `sign`, of which only the last may be followed
/// by the marker that it has no line break.
fn side<'a>(
    lines: &mut std::iter::Peekable<impl Iterator<Item = &'a str>>,
    sign: char,
    count: usize,
) -> Option<String> {
    let mut text = String::new();
    for left in (0..count).rev() {
        let line = lines.next()?.strip_prefix(sign)?;
        if !line.ends_with('\n') {
            return None;
        }
        text.push_str(line);
        if lines
            .next_if_eq(&"\\ No newline at end of file\n")
            .is_some()
        {
            if left > 0 {
                return None;
            }
            text.pop();
        }
    }
    Some(text)
}

/// An edit of a stretch of a file. Its lines are numbered from `first` when
/// the stretch's place in the file is known, and otherwise carry no numbers
/// rather than invented ones.
fn compare(path: String, old: &str, new: &str, first: Option<usize>) -> Change {
    let mut change = Change::bare(Action::Edit, path);
    change.whole_lines = first.is_some();
    let script = edit_script(&split(old), &split(new));
    // Keep CONTEXT unchanged lines beside each change, in one pass over the
    // changes; longer unchanged runs become a gap.
    let mut keep = vec![false; script.len()];
    for (index, (mark, _)) in script.iter().enumerate() {
        match mark {
            Mark::Insert => change.added += 1,
            Mark::Delete => change.removed += 1,
            _ => continue,
        }
        let last = (index + CONTEXT).min(script.len() - 1);
        keep[index.saturating_sub(CONTEXT)..=last].fill(true);
    }
    let (mut gap, mut started) = (false, false);
    // A removed or unchanged line is numbered in the old file, an added one
    // in the new.
    let (mut old_at, mut new_at) = (first.unwrap_or(0), first.unwrap_or(0));
    for ((mark, raw), kept) in script.into_iter().zip(keep) {
        let number = match mark {
            Mark::Insert => new_at,
            _ => old_at,
        };
        match mark {
            Mark::Insert => new_at += 1,
            Mark::Delete => old_at += 1,
            _ => (old_at, new_at) = (old_at + 1, new_at + 1),
        }
        if !kept {
            gap = true;
            continue;
        }
        if gap && started {
            change.rows.push(|| Row::Gap);
        }
        (gap, started) = (false, true);
        let old_number = (mark != Mark::Insert).then_some(old_at.saturating_sub(1));
        let new_number = (mark != Mark::Delete).then_some(new_at.saturating_sub(1));
        change.rows.push(|| {
            let mut row = line(first.map(|_| number), mark, raw);
            if let Row::Line { old, new, .. } = &mut row {
                *old = first.and(old_number);
                *new = first.and(new_number);
            }
            row
        });
    }
    change
}

/// The old lines removed and new lines added, in order, with unchanged lines
/// between them. Common ends are matched first; the middle is compared line by
/// line while that stays small, and is otherwise shown wholly replaced.
fn edit_script<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(Mark, &'a str)> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (old_mid, new_mid) = (
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    );
    let mut script: Vec<(Mark, &str)> = old[..prefix]
        .iter()
        .map(|text| (Mark::Context, *text))
        .collect();
    let cells = (old_mid.len() + 1).saturating_mul(new_mid.len() + 1);
    if cells <= MAX_COMPARED_CELLS {
        script.extend(common_subsequence(old_mid, new_mid));
    } else {
        script.extend(old_mid.iter().map(|text| (Mark::Delete, *text)));
        script.extend(new_mid.iter().map(|text| (Mark::Insert, *text)));
    }
    script.extend(
        old[old.len() - suffix..]
            .iter()
            .map(|text| (Mark::Context, *text)),
    );
    script
}

/// A longest-common-subsequence walk; removals come before additions.
fn common_subsequence<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(Mark, &'a str)> {
    let width = new.len() + 1;
    // `longest[i * width + j]` is the common length of old[i..] and new[j..].
    let mut longest = vec![0u32; (old.len() + 1) * width];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            longest[i * width + j] = if old[i] == new[j] {
                longest[(i + 1) * width + j + 1] + 1
            } else {
                longest[(i + 1) * width + j].max(longest[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut script = Vec::with_capacity(old.len() + new.len());
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            script.push((Mark::Context, old[i]));
            i += 1;
            j += 1;
        } else if i < old.len()
            && (j == new.len() || longest[(i + 1) * width + j] >= longest[i * width + j + 1])
        {
            script.push((Mark::Delete, old[i]));
            i += 1;
        } else {
            script.push((Mark::Insert, new[j]));
            j += 1;
        }
    }
    script
}

#[derive(Default)]
struct Out {
    text: String,
    marks: Vec<Mark>,
}

impl Out {
    fn push(&mut self, row: String, mark: Mark) {
        if !self.marks.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(&row);
        self.marks.push(mark);
    }
}

/// A line as a terminal row: tabs become spaces and other controls are
/// spelled out, so file bytes cannot move the cursor or restyle the screen.
fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\t' => out.push_str("    "),
            ch if ch.is_control() => out.extend(ch.escape_debug()),
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests;
