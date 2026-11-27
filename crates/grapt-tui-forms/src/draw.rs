// Copyright 2025 OpenAI. Licensed under the Apache License, Version 2.0.
// Elicitation context.
// Modified by Grapt: location-owned manual URL consent, inert literal text,
// scrollable full destination and existing choice/cancel routes; no hyperlinks.
use super::*;
use grapt_client::wire::Spelled;
use grapt_tui_transcript::{markdown::literal, palette::Palette};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
impl Board {
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect, palette: Palette) {
        let Some(draft) = self.drafts.get(self.selected) else {
            return;
        };
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(
                " Question {}/{} · F6/Esc close ",
                self.selected + 1,
                self.count()
            ))
            .style(palette.normal());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let footer_height = inner.height.min(if inner.width < 70 { 6 } else { 4 });
        let body = Rect {
            height: inner.height.saturating_sub(footer_height),
            ..inner
        };
        let description = draft
            .row
            .fields
            .get(draft.field)
            .and_then(|field| field.attributes.get("description"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let location = match &draft.target {
            Some(Target::Location(location)) => {
                format!("Location: {}", literal(&location.directory))
            }
            _ => String::new(),
        };
        let message = draft
            .row
            .metadata
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let url_mode = draft
            .row
            .metadata
            .get("kind")
            .and_then(serde_json::Value::as_str)
            == Some("mcp-url-elicitation");
        let url_context = if url_mode {
            let text = |key| {
                draft
                    .row
                    .metadata
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("(not reported)")
            };
            format!(
                "Requesting server: {}\nHost: {}\nURL (manual navigation only):\n{}\nAccept consents to this interaction; it does not open the URL or confirm completion.\nChoose accept or decline, then Ctrl+S. Ctrl+R cancels. PgUp/PgDn reads all details.\n",
                literal(text("server")), literal(text("host")), literal(text("url")))
        } else {
            String::new()
        };
        let context = format!(
            "{}\n{}{}{}Owner: {} · Request: {}\n{}\n{}\n{}",
            literal(&draft.row.title),
            if message.is_empty() {
                String::new()
            } else {
                format!("{}\n", literal(message))
            },
            if location.is_empty() {
                String::new()
            } else {
                format!("{location}\n")
            },
            url_context,
            literal(draft.row.owner.spelled()),
            draft.row.id.spelled(),
            literal(description),
            draft
                .row
                .fields
                .get(draft.field)
                .map(fields::hint)
                .unwrap_or_default(),
            super::choice_draw::option_detail(draft)
        );
        let context = grapt_tui_transcript::markdown::wrap(
            context
                .lines()
                .map(|line| Line::raw(line.to_owned()))
                .collect(),
            inner.width,
        );
        let header_height = body.height.min(if body.height < 7 {
            1
        } else if url_mode {
            body.height.saturating_sub(4)
        } else if matches!(draft.target, Some(Target::Location(_))) {
            body.height.saturating_sub(3).min(6)
        } else {
            3
        });
        let offset = draft
            .scroll
            .min(context.len().saturating_sub(usize::from(header_height)));
        frame.render_widget(
            Paragraph::new(
                context
                    .into_iter()
                    .skip(offset)
                    .take(usize::from(header_height))
                    .collect::<Vec<_>>(),
            )
            .style(palette.normal().fg(palette.accent)),
            Rect {
                height: header_height,
                ..body
            },
        );
        let field_area = Rect {
            y: body.y.saturating_add(header_height),
            height: body.height.saturating_sub(header_height),
            ..body
        };
        let visible = draft.visible_fields();
        let selected = visible
            .iter()
            .position(|index| *index == draft.field)
            .unwrap_or(0);
        let list_height = field_area
            .height
            .saturating_sub(2)
            .max(u16::from(field_area.height > 0))
            .min(visible.len() as u16);
        let start = selected.saturating_sub(usize::from(list_height.saturating_sub(1)));
        let rows = visible
            .iter()
            .skip(start)
            .take(usize::from(list_height))
            .filter_map(|index| draft.row.fields.get(*index).map(|field| (*index, field)))
            .map(|(index, field)| {
                let value = match draft.values.get(&field.key) {
                    Some(FormValue::Text(text)) => literal(text).replace('\n', " ↵ "),
                    Some(FormValue::Flag(flag)) => flag.to_string(),
                    Some(value) => value.to_json().to_string(),
                    None => draft
                        .inputs
                        .get(&field.key)
                        .filter(|text| !text.is_empty())
                        .map(|text| format!("draft: {}", literal(text)))
                        .unwrap_or_else(|| "unanswered".into()),
                };
                Line::styled(
                    grapt_client::cells::cut(
                        &format!(
                            "{} {}{} ({}) = {}",
                            if index == draft.field { ">" } else { " " },
                            literal(field.title.as_deref().unwrap_or(&field.key)),
                            if field.required { " *" } else { "" },
                            literal(&field.kind),
                            value
                        ),
                        usize::from(inner.width),
                    ),
                    palette.normal().fg(if index == draft.field {
                        palette.accent
                    } else {
                        palette.text
                    }),
                )
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(rows),
            Rect {
                height: list_height,
                ..field_area
            },
        );
        let editing = Rect {
            y: field_area.y.saturating_add(list_height),
            height: field_area.height.saturating_sub(list_height),
            ..field_area
        };
        let field = draft.row.fields.get(draft.field);
        let hint = if draft.custom_edit {
            "Type/paste custom text · Enter chooses · Ctrl+E options".into()
        } else {
            field
                .map(fields::hint)
                .unwrap_or_else(|| "No editable fields".into())
        };
        let hint_height = editing.height.min(1);
        frame.render_widget(
            Paragraph::new(grapt_client::cells::cut(
                &literal(&hint),
                usize::from(editing.width),
            ))
            .style(palette.normal().fg(palette.accent)),
            Rect {
                height: hint_height,
                ..editing
            },
        );
        let editing = Rect {
            y: editing.y.saturating_add(hint_height),
            height: editing.height.saturating_sub(hint_height),
            ..editing
        };
        if draft.custom_edit
            || field.is_some_and(|field| {
                matches!(
                    fields::input(field),
                    Ok(fields::Input::Text | fields::Input::Number)
                )
            })
        {
            let (rows, cursor) = crate::editor::rows(&draft.editor, editing.width, palette);
            let top = cursor.saturating_sub(usize::from(editing.height.saturating_sub(1)));
            frame.render_widget(
                Paragraph::new(
                    rows.into_iter()
                        .skip(top)
                        .take(usize::from(editing.height))
                        .collect::<Vec<_>>(),
                )
                .style(palette.normal().bg(palette.panel)),
                editing,
            );
        } else if let Some(field) = field.filter(|field| {
            matches!(
                fields::input(field),
                Ok(fields::Input::Choice | fields::Input::Multi)
            )
        }) {
            super::choice_draw::draw_options(frame, editing, palette, draft, field);
        }
        let state = match &draft.state {
            Some(FormState::Pending) => "Last state: pending".into(),
            Some(FormState::Suspended(reason)) => format!(
                "suspended ({}) · answering will not restart lost work",
                reason.as_str()
            ),
            Some(FormState::Cancelled) => "cancelled".into(),
            Some(FormState::Answered(answer)) => format!(
                "answered: {}",
                answer
                    .iter()
                    .map(|(key, value)| format!(
                        "{}={}",
                        literal(key),
                        match value {
                            FormValue::Text(text) => literal(text),
                            FormValue::Flag(flag) => flag.to_string(),
                            value => value.to_json().to_string(),
                        }
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            None => if matches!(draft.target, Some(Target::Location(_))) {
                "MCP question; waiter liveness unknown · Ctrl+O reads recorded state"
            } else if draft.listed {
                "outstanding; state not yet read"
            } else {
                "not listed; settlement unknown"
            }
            .into(),
        };
        let mut lines = vec![
            Line::raw(grapt_client::cells::cut(&state, usize::from(inner.width))),
            Line::styled(
                grapt_client::cells::cut(
                    &literal(&draft.note).replace('\n', " "),
                    usize::from(inner.width),
                ),
                palette.normal().fg(palette.bad),
            ),
        ];
        if inner.width < 70 {
            lines.extend([
                Line::raw("Tab/Shift+Tab field"),
                Line::raw("Ctrl+N/P question · ↑/↓ option"),
                Line::raw(if url_mode {
                    "Ctrl+S submit decision · Ctrl+R cancel"
                } else {
                    "Ctrl+S answer · Ctrl+R decline"
                }),
                Line::raw("Ctrl+O state · Esc/F6 close"),
            ]);
        } else {
            lines.extend([
                Line::raw(
                    "Tab/Shift+Tab field · Ctrl+N/P question · Space choose · PgUp/PgDn details",
                ),
                Line::raw(if url_mode {
                    "Ctrl+S submit decision · Ctrl+R cancel · Ctrl+O state · Esc/F6 close"
                } else {
                    "Ctrl+S answer · Ctrl+R decline · Ctrl+O state · Esc/F6 close"
                }),
            ]);
        }
        frame.render_widget(
            Paragraph::new(lines).style(palette.normal()),
            Rect {
                y: body.bottom(),
                height: footer_height,
                ..inner
            },
        );
    }
}
