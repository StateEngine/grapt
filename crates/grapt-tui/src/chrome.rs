// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// Modified by Grapt. Contextual footer and transient feedback.
//! Compact session metadata and exceptional feedback fit whole terminal cells.
use crate::ui::Ui;
use grapt_client::cells;
use grapt_tui_transcript::{markdown::literal, palette::Palette};
use ratatui::{
    layout::{Margin, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
mod bypass;
pub use bypass::{draw_bypass, draw_footer, footer_height};

pub fn inner(area: Rect) -> Rect {
    area.inner(Margin::new(1, 0))
}

fn fit(text: &str, width: usize) -> String {
    if cells::cells(text) <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    format!("{}…", cells::cut(text, width - 1))
}

fn title(text: &str) -> String {
    let safe = literal(text).replace('\n', " ");
    if let Some((name, _)) = safe
        .strip_prefix("[seat-runner] You are ")
        .and_then(|rest| rest.split_once(". Seat serial:"))
    {
        let name = name.trim();
        if !name.is_empty() && cells::cells(name) <= 40 {
            return format!("{name} · seat");
        }
    }
    safe
}

pub fn header(frame: &mut Frame<'_>, area: Rect, text: &str, p: Palette) {
    if area.height == 0 {
        return;
    }
    let area = Rect {
        height: 1,
        ..inner(area)
    };
    let left_width = area.width;
    let brand = cells::cut("Grapt  ", usize::from(left_width));
    let text = fit(
        &title(text),
        usize::from(left_width).saturating_sub(cells::cells(&brand)),
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(brand, p.normal().fg(p.accent).add_modifier(Modifier::BOLD)),
            Span::raw(text),
        ])),
        Rect {
            width: left_width,
            ..area
        },
    );
}

fn append(line: &mut String, hint: &str, width: usize) {
    let separator = if line.is_empty() { "" } else { " · " };
    if cells::cells(line) + cells::cells(separator) + cells::cells(hint) <= width {
        line.push_str(separator);
        line.push_str(hint);
    }
}

/// A transient notice leads the row, then the ordinary label takes the space
/// that remains.
///
/// Riskier than [`append`]: the notice survives a narrow terminal by being
/// truncated, because it explains why the screen disagrees with durable
/// history, and a hidden explanation reads as no explanation.
fn leading(notice: Option<&str>, label: &str, width: usize) -> String {
    match notice {
        Some(notice) => {
            let mut line = fit(notice, width);
            let used = cells::cells(&line);
            let room = width.saturating_sub(used + cells::cells(" · "));
            if room >= 8 {
                line.push_str(" · ");
                line.push_str(&fit(label, room));
            }
            line
        }
        None => fit(label, width),
    }
}

fn context_meter(context: &crate::backend::Context) -> Option<String> {
    if context.error.is_some() {
        return None;
    }
    let (used, capacity) = context.used.zip(context.capacity)?;
    if capacity == 0 || u128::from(used) * 100 <= u128::from(capacity) * 70 {
        return None;
    }
    let percent = u128::from(used) * 100 / u128::from(capacity);
    Some(format!("context {percent}%"))
}

fn session_context(ui: &Ui, width: usize) -> String {
    let current = ui.agents.iter().find(|agent| agent.current);
    let directory = ui.directory.as_deref().unwrap_or_default();
    let folder = directory
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(directory);
    let seat = title(&ui.title);
    let agent = seat
        .strip_suffix(" · seat")
        .or_else(|| current.and_then(|agent| agent.display_name.as_deref()));
    let context = [Some(folder), agent]
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let context = literal(&context).replace('\n', " ");
    if context.is_empty() {
        return crate::models::chip(ui.model.as_ref(), width);
    }
    // Keep model and effort legible before spending the remaining cells on labels.
    let model = crate::models::chip(ui.model.as_ref(), width.saturating_sub(4));
    let labels = width.saturating_sub(cells::cells(&model) + 3);
    if labels == 0 || model.is_empty() {
        return crate::models::chip(ui.model.as_ref(), width);
    }
    format!("{} · {model}", fit(&context, labels))
}

pub fn footer(ui: &Ui, width: usize) -> [String; 2] {
    let context = session_context(ui, width);
    let current = ui.agents.iter().find(|agent| agent.current);
    // The gap notice is transient and explains what the screen is showing, so
    // it leads every row rather than being dropped when another label fills
    // the width. `fit` truncates it instead of hiding it outright.
    // The heartbeat leads with the gap notice only when it reports trouble:
    // a live connection and a waiting backend are the absence of a notice.
    let beat =
        (!ui.heartbeat.is_empty() && ui.heartbeat != "connected").then_some(ui.heartbeat.as_str());
    let joined = beat
        .zip(ui.gap_notice.label())
        .map(|(beat, gap)| format!("{beat} · {gap}"));
    let gap = joined.as_deref().or(beat).or_else(|| ui.gap_notice.label());
    if let Some(hint) = ui.quit.hint().or_else(|| ui.clipboard.hint()) {
        return [leading(gap, hint, width), context];
    }
    if let Some(error) = &ui.repository_status {
        return [fit(&literal(error).replace('\n', " "), width), context];
    }
    if let Some(hint) = ui.chords.hint() {
        return [leading(gap, hint, width), context];
    }
    let mail = ui.mail_recovery.label(ui.mail_attention);
    // Degraded continuity shows beside mail, as a state rather than per turn.
    let mail = match (continuity_label(ui.continuity_attention), mail) {
        (Some(continuity), Some(mail)) => Some(format!("{continuity} · {mail}")),
        (continuity, mail) => mail.or_else(|| continuity.map(str::to_owned)),
    };
    if let Some(recall) = ui.recall.status() {
        let line = mail
            .as_ref()
            .map_or_else(|| recall.clone(), |mail| format!("{mail} · {recall}"));
        return [leading(gap, &line, width), context];
    }
    let mut state = String::new();
    if let Some(notice) = gap {
        state.push_str(&fit(notice, width));
    }
    if let Some(mail) = mail {
        append(&mut state, &mail, width);
    }
    let queued = ui
        .queued
        .total
        .saturating_add(ui.queued.steering_total)
        .saturating_add(ui.followups.pending_count())
        .saturating_add(ui.sending.waiting_count());
    if queued > 0 {
        append(&mut state, &format!("{queued} queued"), width);
    }
    let questions = ui.forms.count();
    if questions > 0 && !ui.forms.focused {
        let suffix = if questions == 1 { "" } else { "s" };
        append(&mut state, &format!("{questions} question{suffix}"), width);
    }
    if let Some(meter) = current.and_then(|agent| context_meter(&agent.context)) {
        append(&mut state, &meter, width);
    }
    [state, context]
}

#[cfg(test)]
mod tests;

/// The status-line words for a degraded continuity level; nothing when it is
/// healthy or unknown.
fn continuity_label(
    level: Option<grapt_api::u3a::ContinuityAttentionInfo>,
) -> Option<&'static str> {
    use grapt_api::u3a::{ContinuityAttentionInfo as Level, ContinuityDegradedReason as Reason};
    match level? {
        Level::Ok => None,
        // Only putting the capability again clears this one.
        Level::Degraded {
            reason: Reason::Capability,
        } => Some("continuity: rebind"),
        Level::Degraded { .. } => Some("continuity offline"),
    }
}
