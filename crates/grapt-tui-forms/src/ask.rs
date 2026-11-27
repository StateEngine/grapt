//! Assistant questions share canonical form editing and occupy the composer band.
use super::*;
use grapt_tui_transcript::{markdown::literal, palette::Palette};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

pub(super) fn is_ask(row: &FormRow) -> bool {
    row.metadata.get("kind").and_then(serde_json::Value::as_str) == Some("request_user_input")
}
impl Board {
    pub fn input_band(&self) -> bool {
        self.focused
            && self
                .drafts
                .get(self.selected)
                .is_some_and(|d| is_ask(&d.row) && d.listed)
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn draw_ask(&self, frame: &mut Frame<'_>, area: Rect, p: Palette) {
        let Some(draft) = self.drafts.get(self.selected) else {
            return;
        };
        let Some(field) = draft.row.fields.get(draft.field) else {
            return;
        };
        frame.render_widget(Clear, area);
        let answered = draft.values.len();
        let block = Block::default()
            .borders(Borders::ALL)
            .style(p.normal())
            .title(format!(
                " {} · question {}/{} · {answered} answered ",
                literal(field.title.as_deref().unwrap_or("Assistant question")),
                draft.field + 1,
                draft.row.fields.len(),
            ));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let footer = inner.height.min(3);
        let mut body = Rect {
            height: inner.height.saturating_sub(footer),
            ..inner
        };
        let question = field
            .attributes
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let lines =
            grapt_tui_transcript::markdown::wrap(vec![Line::raw(literal(question))], inner.width);
        let header = body.height.min(3).min(lines.len() as u16);
        let offset = draft
            .scroll
            .min(lines.len().saturating_sub(usize::from(header)));
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(offset)
                    .take(usize::from(header))
                    .collect::<Vec<_>>(),
            )
            .style(p.normal().fg(p.accent)),
            Rect {
                height: header,
                ..body
            },
        );
        body.y = body.y.saturating_add(header);
        body.height = body.height.saturating_sub(header);
        let details = body.height.min(2);
        let editing = Rect {
            height: body.height.saturating_sub(details),
            ..body
        };
        if draft.custom_edit {
            let (rows, cursor) = crate::editor::rows(&draft.editor, editing.width, p);
            let top = cursor.saturating_sub(usize::from(editing.height.saturating_sub(1)));
            frame.render_widget(
                Paragraph::new(
                    rows.into_iter()
                        .skip(top)
                        .take(usize::from(editing.height))
                        .collect::<Vec<_>>(),
                )
                .style(p.normal().bg(p.panel)),
                editing,
            );
        } else {
            super::choice_draw::draw_options(frame, editing, p, draft, field);
        }
        let cursor = draft.choices.get(&field.key).copied().unwrap_or(0);
        let detail = fields::options(field)
            .get(cursor)
            .and_then(|option| option["description"].as_str())
            .unwrap_or("");
        let detail =
            grapt_tui_transcript::markdown::wrap(vec![Line::raw(literal(detail))], inner.width);
        frame.render_widget(
            Paragraph::new(detail).style(p.normal().fg(p.muted)),
            Rect {
                y: editing.bottom(),
                height: details,
                ..body
            },
        );
        let hint = if draft.pending {
            "Sending answer…"
        } else if draft.custom_edit {
            "Other: type your answer · Enter chooses · Ctrl+E options"
        } else if field.kind == "multiselect" {
            "↑/↓ move · Space toggles · Ctrl+E Other (write an answer)"
        } else {
            "↑/↓ move · Space/Enter chooses · Ctrl+E Other (write an answer)"
        };
        let lines = vec![
            Line::styled(
                grapt_client::cells::cut(&literal(&draft.note), usize::from(inner.width)),
                p.normal().fg(p.accent),
            ),
            Line::raw(hint),
            Line::raw("Tab/Shift+Tab question · Ctrl+S send all answers · Esc decline"),
        ];
        frame.render_widget(
            Paragraph::new(lines).style(p.normal()),
            Rect {
                y: inner.bottom().saturating_sub(footer),
                height: footer,
                ..inner
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    #[test]
    fn assistant_question_escape_declines_and_settlement_releases_the_input_band(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut row = crate::tests::row("frm_assistant")?;
        row.metadata
            .insert("kind".into(), serde_json::json!("request_user_input"));
        let session = grapt_client::wire::session_id("ses_forms")?;
        let mut board = Board::default();
        board.update(Some(session.clone()), None, vec![row.clone()]);
        assert!(board.input_band());
        let request = board
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .ok_or("decline not sent")?;
        assert_eq!(request.decision, Some(Decision::Cancel));
        assert!(board.reply(Reply {
            target: request.target,
            form: row.id.clone(),
            state: Some(FormState::Cancelled),
            validation_refused: false,
            note: String::new(),
        }));
        assert!(!board.input_band());
        board.update(Some(session.clone()), None, Vec::new());
        assert_eq!(board.count(), 0);

        board.update(Some(session.clone()), None, vec![row.clone()]);
        board.update(Some(session.clone()), None, Vec::new());
        assert!(!board.focused, "a removed question has no live waiter");
        assert!(!board.input_band());
        assert_eq!(board.count(), 0);

        board.update(Some(session.clone()), None, vec![row.clone()]);
        let unconfirmed = board
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .ok_or("unconfirmed decline not sent")?;
        board.update(Some(session.clone()), None, Vec::new());
        assert!(!board.focused);
        assert!(!board.input_band());
        assert_eq!(
            board.count(),
            1,
            "keep an uncertain decision for reconciliation"
        );
        board.reply(Reply {
            target: unconfirmed.target,
            form: row.id,
            state: Some(FormState::Cancelled),
            validation_refused: false,
            note: String::new(),
        });
        board.update(Some(session), None, Vec::new());
        assert_eq!(board.count(), 0);
        Ok(())
    }
}
