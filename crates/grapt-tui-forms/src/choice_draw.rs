//! Option display for elicitation forms.
//! Copyright 2025 OpenAI. Apache-2.0; see licenses/openai-codex/
//! Modified by Grapt: bounded offered/custom selection and separate explicit text entry.
//! Choice details and bounded option presentation.
use super::*;
use grapt_tui_transcript::{markdown::literal, palette::Palette};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
    Frame,
};

pub(super) fn option_detail(draft: &Draft) -> String {
    let Some(field) = draft.row.fields.get(draft.field) else {
        return String::new();
    };
    let cursor = draft.choices.get(&field.key).copied().unwrap_or(0);
    let values = draft.choice_values(field);
    let Some(value) = values.get(cursor) else {
        return String::new();
    };
    let option = fields::options(field).get(cursor);
    format!(
        "Option {}: {}\nValue: {}\n{}",
        cursor + 1,
        literal(
            option
                .and_then(|option| option["label"].as_str())
                .unwrap_or("Custom value")
        ),
        literal(value),
        literal(
            option
                .and_then(|option| option["description"].as_str())
                .unwrap_or("")
        )
    )
}

pub(super) fn draw_options(
    frame: &mut Frame<'_>,
    area: Rect,
    palette: Palette,
    draft: &Draft,
    field: &grapt_client::wire::FormField,
) {
    let cursor = draft.choices.get(&field.key).copied().unwrap_or(0);
    let start = cursor.saturating_sub(usize::from(area.height.saturating_sub(1)));
    let values = draft.choice_values(field);
    let rows = values
        .iter()
        .enumerate()
        .skip(start)
        .take(usize::from(area.height))
        .map(|(index, value)| {
            let value = value.as_str();
            let label = fields::options(field)
                .get(index)
                .and_then(|option| option["label"].as_str())
                .unwrap_or("Custom");
            let chosen = match draft.values.get(&field.key) {
                Some(FormValue::Text(text)) => text == value,
                Some(FormValue::Texts(items)) => items.iter().any(|item| item == value),
                _ => false,
            };
            let named = if label == value {
                literal(label)
            } else {
                format!("{} · {}", literal(label), literal(value))
            };
            Line::styled(
                grapt_client::cells::cut(
                    &format!(
                        "{} [{}] {}. {}",
                        if index == cursor { ">" } else { " " },
                        if chosen { "x" } else { " " },
                        index + 1,
                        named
                    ),
                    usize::from(area.width),
                ),
                palette.normal().fg(if index == cursor {
                    palette.accent
                } else {
                    palette.text
                }),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(rows).wrap(Wrap { trim: false }), area);
}
