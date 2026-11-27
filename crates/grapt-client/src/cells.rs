//! How many terminal CELLS a string occupies.
//!
//! A frame line is laid out in cells and the renderer measured it in Rust
//! `char`s. Those are different quantities: a wide glyph is one `char` and two
//! cells, so a line believed to fit could be a cell wider than its pane for
//! every wide glyph in it.
//!
//! The emoji ranges are deliberately TWO cells, and must not be narrowed to
//! agree with a capture that reports one. `U+1F642` is `East_Asian_Width=Wide`
//! with `Emoji_Presentation=Yes`; a grid advancing one cell for it is behind
//! that convention, and matching it here would break this client against every
//! terminal that gets it right. See `GLYPH-DISCRIMINATOR-01.md`.
//!
//! ## One measure for the frame, the composer and the screen buffer
//!
//! A terminal draws GLYPHS, not characters: `e` with a combining accent, `☀`
//! with its emoji selector, `👍` with its skin tone, a family joined by
//! zero-width joiners or a pair of regional indicators is one glyph in one
//! run of cells. [`glyphs`] splits text into extended grapheme clusters
//! (`unicode-segmentation`, UAX #29) and measures each as a whole string with
//! `unicode-width` (UAX #11 with emoji presentation), which is exactly how the
//! screen buffer (`ratatui-core`) measures a cell's symbol when it diffs.
//! Every measure here — [`cells`], [`cut`], [`tail`], [`wrap`] — and every
//! style column is taken from [`glyphs`], so the frame, the composer's cursor
//! and the buffer agree about where each glyph ends. Measuring per character
//! instead put the `x` of `☀️x` in the column the sun still covers.
//!
//! Two exceptions follow terminals rather than the table: a control
//! character is drawn as a one-cell replacement, and a halfwidth (semi-)voiced
//! sound mark (U+FF9E, U+FF9F) takes a cell of its own.
//!
//! `unicode-width` is admitted by the maintainer's 10-18 TUI dependency ruling
//! (SC-TUI-01), and `unicode-segmentation` by a ruling under it; they
//! replaced a bounded hand-written table.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Longest glyph kept whole, in bytes: a cluster of combining marks past it
/// continues as zero-cell glyphs rather than growing one symbol without bound.
const MAX_GLYPH_BYTES: usize = 64;

fn is_sound_mark(ch: char) -> bool {
    matches!(ch, '\u{ff9e}' | '\u{ff9f}')
}

/// Drawn on its own: a control (as a replacement) or a sound mark.
fn stands_alone(ch: char) -> bool {
    ch.is_control() || is_sound_mark(ch)
}

/// The cells one glyph occupies.
fn glyph_cells(glyph: &str) -> usize {
    match glyph.chars().next() {
        Some(ch) if stands_alone(ch) => 1,
        _ => glyph.width(),
    }
}

/// The glyphs of one grapheme cluster that starts at `offset`: the cluster
/// itself, unless a control or sound mark in it stands alone or it outgrows
/// [`MAX_GLYPH_BYTES`].
fn pieces(offset: usize, grapheme: &str) -> impl Iterator<Item = (usize, &str, usize)> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        let rest = grapheme.get(at..).filter(|rest| !rest.is_empty())?;
        let mut chars = rest.char_indices();
        let (_, first) = chars.next()?;
        let mut end = first.len_utf8();
        if !stands_alone(first) {
            for (index, ch) in chars {
                if stands_alone(ch) || index + ch.len_utf8() > MAX_GLYPH_BYTES {
                    break;
                }
                end = index + ch.len_utf8();
            }
        }
        let piece = &rest[..end];
        let start = offset + at;
        at += end;
        Some((start, piece, glyph_cells(piece)))
    })
}

/// The glyphs of `text` in order: each one's byte offset, its text and the
/// cells it occupies. A glyph of zero cells is a mark with nothing to attach
/// to.
pub fn glyphs(text: &str) -> impl Iterator<Item = (usize, &str, usize)> + '_ {
    text.grapheme_indices(true)
        .flat_map(|(offset, grapheme)| pieces(offset, grapheme))
}

/// `text` without its leading whitespace glyphs. A space carrying a mark is a
/// glyph of its own and stays whole.
fn trim_spaces(text: &str) -> &str {
    let start = glyphs(text)
        .find(|(_, glyph, _)| !glyph.chars().all(char::is_whitespace))
        .map_or(text.len(), |(at, _, _)| at);
    &text[start..]
}

/// The cells a string occupies.
pub fn cells(text: &str) -> usize {
    glyphs(text).map(|(_, _, cells)| cells).sum()
}

/// Cut a string to at most `width` CELLS, never splitting a glyph across the
/// boundary.
///
/// A wide glyph that would straddle the edge is dropped rather than half
/// drawn: half of a two-cell glyph is not half a character, it is a cell of
/// garbage that shifts everything after it.
pub fn cut(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for (_, glyph, cost) in glyphs(text) {
        if used + cost > width {
            break;
        }
        out.push_str(glyph);
        used += cost;
    }
    out
}

/// The last `width` CELLS of a string — the mirror of [`cut`].
///
/// For content whose meaning sits at the end of the line: the faulting file
/// is the TAIL of the product's refusal message, so a refusal that no longer
/// fits keeps its tail rather than the opening words that name nothing the
/// user can act on. Like `cut`, a wide glyph is dropped rather than split.
pub fn tail(text: &str, width: usize) -> String {
    let all: Vec<(usize, &str, usize)> = glyphs(text).collect();
    let mut start = text.len();
    let mut used = 0usize;
    for (at, _, cost) in all.into_iter().rev() {
        if used + cost > width {
            break;
        }
        start = at;
        used += cost;
    }
    text[start..].to_owned()
}

/// Break one logical line into rows of at most `width` cells.
///
/// Prefers to break at a space so words survive, and falls back to a hard cell
/// break when a single run is longer than the pane -- a URL, a JSON blob, a
/// path.
///
/// THREE PROPERTIES, each of which was violated by an earlier revision and is
/// now witnessed:
///
/// - **No row exceeds `width`.** The space retreat used to carry a remainder
///   forward that left no room for the glyph which forced the break, producing
///   a row wider than the pane. The renderer's `cut` then silently removed the
///   overflow, so a wrap written to stop content being clipped was clipping it.
/// - **No content is lost.** Every character of the input appears in the
///   output, except one that cannot fit a whole empty row of this width, which
///   becomes a single-cell replacement marker. Dropping it would lose content
///   silently and emitting it would break the bound above; a marker keeps both
///   promises and tells the reader something is there.
/// - **No empty row is invented.** Retreating to a space at the very start of
///   a row emitted a blank row and carried the whole line forward, which is a
///   break that breaks nothing.
///
/// An empty input yields ONE empty row rather than none, because a blank line
/// in a prompt is a line the user typed and dropping it reflows their text.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    let mut used = 0usize;
    // The byte offset of the last space in the current row, so a break can
    // retreat to it.
    let mut break_at: Option<usize> = None;

    for (_, original, cost) in glyphs(text) {
        // A glyph too wide for a whole empty row can never be placed. It is
        // shown as a one-cell marker rather than dropped or overflowed.
        let (glyph, cost) = match cost {
            cost if cost > width => ("\u{fffd}", 1),
            cost => (original, cost),
        };

        if used + cost > width && !row.is_empty() {
            // Retreat to the last space, but only when the text BEFORE it
            // occupies at least one cell.
            //
            // This tested the space's BYTE offset, and a positive byte offset
            // does not mean a cell was used: a zero-width mark before the
            // space makes the offset positive while the space still sits in
            // column zero. The retreat then carried the whole row forward and
            // left a row of nothing behind.
            let prefix_cells = break_at.map_or(0, |byte| cells(&row[..byte]));
            match break_at {
                Some(byte) if prefix_cells > 0 && byte < row.len() => {
                    let carry: String = trim_spaces(&row[byte..]).to_owned();
                    row.truncate(byte);
                    rows.push(std::mem::take(&mut row));
                    row = carry;
                    used = cells(&row);
                }
                _ => {
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
            }
            break_at = None;
            // AND RE-CHECK IN CELLS, deliberately kept.
            //
            // A mutation removing this currently SURVIVES. That is not a
            // licence to delete it: an earlier revision did exactly that, on
            // exactly that reasoning, and shipped a bound violation a
            // zero-width mark could reach. Survival means the witnesses do not
            // discriminate the branch, not that nothing can.
            //
            // It re-asserts the bound the loop above is supposed to have
            // established, in the same units the bound is stated in.
            if used + cost > width && !row.is_empty() {
                rows.push(std::mem::take(&mut row));
                used = 0;
            }
        }
        if glyph == " " {
            break_at = Some(row.len());
        }
        row.push_str(glyph);
        used += cost;
    }
    rows.push(row);
    rows
}

// Apache-2.0, Copyright 2025 OpenAI.
/// Shorten at word boundaries, reserving one cell for an ellipsis.
pub fn ellipsize_words(text: &str, width: usize) -> String {
    if cells(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut line = String::new();
    for word in text.split_whitespace() {
        let separator = usize::from(!line.is_empty());
        if cells(&line) + separator + cells(word) >= width {
            break;
        }
        if separator != 0 {
            line.push(' ');
        }
        line.push_str(word);
    }
    line.push('…');
    line
}

#[cfg(test)]
#[path = "cells/tests.rs"]
mod tests;
