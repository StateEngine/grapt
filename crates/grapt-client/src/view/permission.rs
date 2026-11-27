//! Literal approval context shared by terminal presentations.
//! Apache-2.0, Copyright OpenAI.
use super::approval;
use crate::client::PermissionAsk;
use crate::style::{Style, Token};
use crate::wire::Spelled;
use grapt_api::u3a::{PermissionPatchFormatDto, PermissionPreviewDto};

/// Safely wrapped service feedback followed by the literal approval context.
pub fn review_rows_with_notice(
    ask: &PermissionAsk,
    notice: Option<&str>,
    width: usize,
) -> Vec<(String, Style)> {
    let mut rows = Vec::new();
    if let Some(notice) = notice {
        append(&mut rows, &format!("Notice: {notice}"), width);
    }
    rows.extend(review_rows(ask, width));
    rows
}

/// A short action preview; full literal context remains available on expansion.
pub fn compact_rows(
    ask: &PermissionAsk,
    notice: Option<&str>,
    width: usize,
) -> Vec<(String, Style)> {
    let mut rows = Vec::new();
    if let Some(notice) = notice {
        append(&mut rows, notice, width);
        rows.truncate(2);
    }
    let mut explanation = Vec::new();
    append(&mut explanation, &ask.subtitle.replace('\n', " "), width);
    rows.extend(explanation.into_iter().take(1));
    let mut preview = Vec::new();
    match ask.preview.as_ref() {
        Some(PermissionPreviewDto::Shell(value)) => append_styled(
            &mut preview,
            value.command().get().as_str(),
            width,
            Style::token(Token::Code),
        ),
        Some(PermissionPreviewDto::Patch(value)) => {
            for line in value.patch().get().as_str().split('\n') {
                let style = if *value.format().get() == PermissionPatchFormatDto::Operations {
                    Style::PLAIN
                } else {
                    approval::patch_style(line)
                };
                append_styled(&mut preview, line, width, style);
            }
        }
        Some(PermissionPreviewDto::Unavailable(value)) => append(
            &mut preview,
            &format!("Preview unavailable: {}", value.reason().get().as_str()),
            width,
        ),
        None => append(&mut preview, "Preview unavailable for this request.", width),
    }
    let more = preview.len() > 3;
    rows.extend(preview.into_iter().take(3));
    if more {
        append(&mut rows, "… F4 to review the full preview", width);
    }
    rows
}

/// Literal, safely wrapped approval context shared by terminal presentations.
/// Decision controls and viewport state belong to each frontend.
pub fn review_rows(ask: &PermissionAsk, width: usize) -> Vec<(String, Style)> {
    let mut rows = Vec::new();
    append(&mut rows, &format!("Action: {}", ask.title), width);
    append(&mut rows, &format!("Explanation: {}", ask.subtitle), width);
    append(
        &mut rows,
        &format!(
            "Session: {}\nRequest: {}",
            ask.session_id.spelled(),
            ask.request_id.spelled()
        ),
        width,
    );
    match ask.preview.as_ref() {
        Some(PermissionPreviewDto::Shell(preview)) => {
            append(
                &mut rows,
                &format!("Working directory: {}", preview.cwd().get().as_str()),
                width,
            );
            append(
                &mut rows,
                &format!("Shell: {}", preview.shell().get().as_str()),
                width,
            );
            if let Some(verdict) = preview.classifier_verdict().as_present() {
                append(
                    &mut rows,
                    &format!("Classification: {}", verdict.as_str()),
                    width,
                );
            }
            append(&mut rows, "Command (controls escaped):", width);
            append_styled(
                &mut rows,
                preview.command().get().as_str(),
                width,
                Style::token(Token::Code),
            );
            if *preview.truncated().get() {
                append(
                    &mut rows,
                    "Source preview truncated; remaining command unavailable.",
                    width,
                );
            }
        }
        Some(PermissionPreviewDto::Patch(preview)) => {
            let format = match preview.format().get() {
                PermissionPatchFormatDto::ApplyPatch => "apply-patch",
                PermissionPatchFormatDto::Operations => "operations",
                PermissionPatchFormatDto::Replacement => "replacement",
                PermissionPatchFormatDto::Write => "write",
            };
            append(&mut rows, &format!("Patch format: {format}"), width);
            append(&mut rows, "Patch (controls escaped):", width);
            for line in preview.patch().get().as_str().split('\n') {
                let style = if *preview.format().get() == PermissionPatchFormatDto::Operations {
                    Style::PLAIN
                } else {
                    approval::patch_style(line)
                };
                append_styled(&mut rows, line, width, style);
            }
            if *preview.truncated().get() {
                append(
                    &mut rows,
                    "Source preview truncated; remaining patch unavailable.",
                    width,
                );
            }
        }
        Some(PermissionPreviewDto::Unavailable(preview)) => {
            append(
                &mut rows,
                &format!("Preview unavailable: {}", preview.reason().get().as_str()),
                width,
            );
        }
        None => append(&mut rows, "Preview unavailable for this request.", width),
    }
    rows
}

/// Whether the stored preview contains only a prefix of the proposed effect.
pub fn source_truncated(ask: &PermissionAsk) -> bool {
    match ask.preview.as_ref() {
        Some(PermissionPreviewDto::Shell(value)) => *value.truncated().get(),
        Some(PermissionPreviewDto::Patch(value)) => *value.truncated().get(),
        _ => false,
    }
}

fn append(rows: &mut Vec<(String, Style)>, text: &str, width: usize) {
    append_styled(rows, text, width, Style::PLAIN);
}

// Unlike prose wrapping, literal review must retain indentation, repeated
// spaces, explicit blank lines and trailing spaces. Escape terminal controls
// before measuring; a command must never be able to repaint its own preview.
fn append_styled(rows: &mut Vec<(String, Style)>, text: &str, width: usize, style: Style) {
    let width = width.max(1);
    for source in text.split('\n') {
        let mut safe = String::new();
        for ch in source.chars() {
            if ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                safe.extend(ch.escape_default());
            } else {
                safe.push(ch);
            }
        }
        let mut line = String::new();
        let mut used = 0usize;
        for (_, glyph, cells) in crate::cells::glyphs(&safe) {
            let (glyph, cells) = if cells > width {
                ("�", 1)
            } else {
                (glyph, cells)
            };
            if used + cells > width && !line.is_empty() {
                rows.push((std::mem::take(&mut line), style));
                used = 0;
            }
            line.push_str(glyph);
            used += cells;
        }
        rows.push((line, style));
    }
}

#[cfg(test)]
mod tests;
