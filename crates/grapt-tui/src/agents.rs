//! Explicit offered-agent selection follows the shared frontend's picker flow.
//! List presentation.
//! Apache-2.0, Copyright 2025 OpenAI.
//! Modified by Grapt for bounded typed roster snapshots and existing commands.
//! A selected row's diagnostic longer than the detail band scrolls with
//! PageUp and PageDown.
use crossterm::event::KeyCode;
use grapt_client::wire::{AgentRow, AgentWireId};
use grapt_tui_transcript::{markdown::literal, palette::Palette};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};

/// A roster item is at most two wrapped lines. An oversized item overflows
/// the viewport, and Ratatui's List then computes an empty render range and
/// hides the selected agent; the remainder stays readable in the detail
/// band below the list.
const ROW_MAX_LINES: usize = 2;
/// Wrapped lines the detail band shows at once. A note longer than the band
/// scrolls with PageUp and PageDown, keeping its tail reachable.
const DETAIL_MAX_LINES: usize = 6;
/// Rows the list keeps whenever the pane is tall enough for the band.
const LIST_MIN_ROWS: u16 = 2;

pub struct Picker {
    rows: Vec<AgentRow>,
    selected: usize,
    page: usize,
    pub pending: bool,
    detail_lines: Vec<String>,
    detail_band: usize,
    detail_scroll: usize,
}
impl Picker {
    pub fn new(rows: Vec<AgentRow>) -> Self {
        Self {
            rows,
            selected: 0,
            page: 1,
            pending: false,
            detail_lines: Vec::new(),
            detail_band: 0,
            detail_scroll: 0,
        }
    }
    pub fn selected(&self) -> Option<&AgentWireId> {
        self.rows.get(self.selected).map(|row| &row.id)
    }
    pub fn key(&mut self, key: KeyCode) {
        let last = self.rows.len().saturating_sub(1);
        // A diagnostic taller than the detail band pages itself first;
        // otherwise the page keys page the roster.
        let note = self.detail_lines.len() > self.detail_band;
        let step = self.detail_band.max(1);
        let max_scroll = self.detail_lines.len().saturating_sub(self.detail_band);
        let selected = match key {
            KeyCode::Up => self.selected.saturating_sub(1),
            KeyCode::Down => self.selected.saturating_add(1).min(last),
            KeyCode::PageUp if note => self.selected,
            KeyCode::PageDown if note => self.selected,
            KeyCode::PageUp => self.selected.saturating_sub(self.page),
            KeyCode::PageDown => self.selected.saturating_add(self.page).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => self.selected,
        };
        if selected != self.selected {
            // A new selection starts its diagnostic at the first line.
            self.selected = selected;
            self.detail_scroll = 0;
        } else if note {
            self.detail_scroll = match key {
                KeyCode::PageUp => self.detail_scroll.saturating_sub(step),
                KeyCode::PageDown => (self.detail_scroll + step).min(max_scroll),
                _ => self.detail_scroll,
            };
        }
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, palette: Palette) {
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(if self.pending {
                " Selecting agent… "
            } else {
                " Agents · Enter select · Esc close "
            })
            .style(palette.normal().bg(palette.panel));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if self.rows.is_empty() {
            frame.render_widget(
                Paragraph::new("No agents offered; Esc then /agents to reopen"),
                inner,
            );
            return;
        }
        // Wrap at the pane width minus the highlight symbol so an appended
        // diagnostic stays readable in a narrow pane.
        let width = inner.width.saturating_sub(2).max(1) as usize;
        let row_texts: Vec<String> = self
            .rows
            .iter()
            .map(|row| {
                let name = format!(
                    "{}{}",
                    literal(&row.name).replace('\n', " "),
                    claude_source_badge(row.source.as_deref())
                );
                match row.description.as_deref() {
                    Some(description) if !description.is_empty() => {
                        format!("{name}  {}", literal(description).replace('\n', " "))
                    }
                    _ => name,
                }
            })
            .collect();
        let detail_lines: Vec<String> = if self
            .rows
            .get(self.selected)
            .is_some_and(|row| row.description.as_deref().is_some_and(|d| !d.is_empty()))
        {
            row_texts
                .get(self.selected)
                .map(|text| wrap_row(text, width))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let detail_band = detail_lines
            .len()
            .min(DETAIL_MAX_LINES)
            .min(inner.height.saturating_sub(LIST_MIN_ROWS) as usize);
        let list_height = inner.height - detail_band as u16;
        self.page = usize::from(list_height).max(1);
        self.detail_scroll = self
            .detail_scroll
            .min(detail_lines.len().saturating_sub(detail_band));
        let list_rect = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: list_height,
        };
        let rows = row_texts.iter().map(|text| {
            let wrapped = wrap_row(text, width);
            let mut lines: Vec<Line> = wrapped
                .iter()
                .take(ROW_MAX_LINES)
                .map(|line| Line::from(line.clone()))
                .collect();
            if wrapped.len() > ROW_MAX_LINES {
                let trimmed =
                    truncate_to_width(&wrapped[ROW_MAX_LINES - 1], width.saturating_sub(1).max(1));
                if let Some(cell) = lines.last_mut() {
                    *cell = Line::from(trimmed + "…");
                }
            }
            ListItem::new(lines)
        });
        frame.render_stateful_widget(
            List::new(rows)
                .highlight_symbol("› ")
                .highlight_style(palette.normal().fg(palette.accent)),
            list_rect,
            &mut ListState::default().with_selected(Some(self.selected)),
        );
        if detail_band > 0 {
            let detail_rect = Rect {
                x: inner.x,
                y: inner.y + list_height,
                width: inner.width,
                height: detail_band as u16,
            };
            let window: Vec<Line> = detail_lines
                .iter()
                .skip(self.detail_scroll)
                .take(detail_band)
                .map(|line| Line::from(line.clone()))
                .collect();
            frame.render_widget(Paragraph::new(window), detail_rect);
        }
        self.detail_lines = detail_lines;
        self.detail_band = detail_band;
    }
}

/// Only a row the product loaded from a Claude-format agent file gets the badge:
/// that is the case where the user must know which definition won a
/// same-name conflict. Mirrors the shared frontend's badge (private there).
fn claude_source_badge(source: Option<&str>) -> String {
    source
        .filter(|source| source.starts_with("claude"))
        .map(|source| format!(" [{source}]"))
        .unwrap_or_default()
}

/// Wrap a roster row to the pane width so an appended diagnostic stays
/// readable: words break on spaces, one overlong word on characters.
fn wrap_row(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_width = 0usize;
    for word in text.split(' ') {
        // The row joins name and description with a double space; an empty
        // token must not consume a line of its own.
        if word.is_empty() {
            continue;
        }
        let word_width = unicode_width::UnicodeWidthStr::width(word);
        let sep_width = usize::from(!line.is_empty());
        if line_width + sep_width + word_width <= width {
            if !line.is_empty() {
                line.push(' ');
                line_width += 1;
            }
            line.push_str(word);
            line_width += word_width;
        } else if word_width <= width {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            line.push_str(word);
            line_width = word_width;
        } else {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let mut chunk_width = 0usize;
            for ch in word.chars() {
                let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if chunk_width + char_width > width && chunk_width > 0 {
                    lines.push(line.clone());
                    line.clear();
                    chunk_width = 0;
                }
                line.push(ch);
                chunk_width += char_width;
            }
            line_width = chunk_width;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Cut a wrapped line to the pane width by display cells; the row cap
/// appends the ellipsis the cut leaves behind.
fn truncate_to_width(line: &str, width: usize) -> String {
    let mut out = String::new();
    let mut used = 0usize;
    for ch in line.chars() {
        let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + char_width > width {
            break;
        }
        out.push(ch);
        used += char_width;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backend::{Backend, Command},
        keys,
        ui::Ui,
    };
    use crossterm::event::{KeyEvent, KeyModifiers};
    use grapt_client::{
        Bounds, Client, Connection, ContractDouble, FramePoll, Operation, Transport,
        TransportError, WireRequest, WireResponse,
    };
    use ratatui::{backend::TestBackend, Terminal};
    use std::sync::mpsc;

    #[derive(Debug)]
    struct EmptyRoster {
        inner: ContractDouble,
        reads: usize,
        empty: bool,
    }

    impl Transport for EmptyRoster {
        fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
            let mut response = self.inner.call(request)?;
            if request.operation() == Operation::AgentList && self.empty {
                self.reads += 1;
                let mut body: serde_json::Value =
                    serde_json::from_slice(&response.body).map_err(|error| TransportError::Io {
                        reason: error.to_string(),
                    })?;
                body["data"] = serde_json::json!([]);
                response.body = serde_json::to_vec(&body).map_err(|error| TransportError::Io {
                    reason: error.to_string(),
                })?;
            }
            Ok(response)
        }
    }

    impl Connection for EmptyRoster {
        fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
            self.inner.poll_frame()
        }

        fn subscribed(&self) -> bool {
            self.inner.subscribed()
        }

        fn resubscribe(&mut self) {
            self.inner.resubscribe();
        }
    }

    #[test]
    fn the_picker_badges_claude_rows_and_shows_their_dropped_tools_note(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rows = vec![
            AgentRow {
                id: grapt_client::wire::agent_id("agt_reviewer")?,
                name: "Reviewer".into(),
                description: Some("Reviews diffs".into()),
                mode: grapt_api::u3a::AgentInfoDtoMode::Primary,
                hidden: false,
                source: Some("user".into()),
            },
            AgentRow {
                id: grapt_client::wire::agent_id("agt_guard")?,
                name: "Guard".into(),
                description: Some("Guard; dropped unknown tools: Bash, Grep".into()),
                mode: grapt_api::u3a::AgentInfoDtoMode::Primary,
                hidden: false,
                source: Some("claude-user".into()),
            },
        ];
        let mut picker = Picker::new(rows);
        let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
        terminal.draw(|frame| picker.draw(frame, frame.area(), Palette::new(false)))?;
        let shown: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(shown.contains("Reviewer"));
        assert!(shown.contains("[claude-user]"));
        assert!(shown.contains("dropped unknown tools: Bash, Grep"));
        assert!(!shown.contains("[user]"));
        Ok(())
    }

    fn capture_screen(picker: &mut Picker, terminal: &mut Terminal<TestBackend>) -> String {
        terminal
            .draw(|frame| picker.draw(frame, frame.area(), Palette::new(false)))
            .unwrap_or_else(|error| panic!("picker draw failed: {error}"));
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn the_picker_keeps_appended_diagnostics_readable_in_a_narrow_pane(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The long description mirrors the data layer's bounded row text:
        // a 1024-byte budget with the diagnostic kept at the tail.
        let long_description = format!("{}…; dropped unknown tools: Bash", "d".repeat(1000));
        let long_note = concat!(
            "Noted; dropped unknown tools: Apple, Banana, Cherry, Date, Elder, ",
            "Fox, Golf, Hotel, India, Juliet",
        );
        let rows = vec![
            AgentRow {
                id: grapt_client::wire::agent_id("agt_guard")?,
                name: "Guard".into(),
                description: Some("Guard; dropped unknown tools: Bash, Grep".into()),
                mode: grapt_api::u3a::AgentInfoDtoMode::Primary,
                hidden: false,
                source: Some("claude-user".into()),
            },
            AgentRow {
                id: grapt_client::wire::agent_id("agt_long")?,
                name: "Long".into(),
                description: Some(long_description),
                mode: grapt_api::u3a::AgentInfoDtoMode::Primary,
                hidden: false,
                source: Some("claude-user".into()),
            },
            AgentRow {
                id: grapt_client::wire::agent_id("agt_noted")?,
                name: "Noted".into(),
                description: Some(long_note.into()),
                mode: grapt_api::u3a::AgentInfoDtoMode::Primary,
                hidden: false,
                source: Some("claude-user".into()),
            },
        ];
        let mut picker = Picker::new(rows);
        let mut terminal = Terminal::new(TestBackend::new(24, 10))?;
        // A note that fits the band stays fully readable without scrolling.
        let shown = capture_screen(&mut picker, &mut terminal);
        for fragment in [
            "Guard",
            "[claude-user]",
            "dropped",
            "unknown",
            "tools:",
            "Bash,",
            "Grep",
        ] {
            assert!(shown.contains(fragment), "narrow pane lost {fragment}");
        }
        // A diagnostic longer than the band: the head is visible at once and
        // the tail stays reachable by scrolling the band.
        picker.key(KeyCode::Down);
        let shown = capture_screen(&mut picker, &mut terminal);
        for fragment in ["Long", "[claude-user]", "ddddd"] {
            assert!(
                shown.contains(fragment),
                "long row lost its head {fragment}"
            );
        }
        for _ in 0..8 {
            picker.key(KeyCode::PageDown);
        }
        let shown = capture_screen(&mut picker, &mut terminal);
        for fragment in ["dropped", "unknown", "tools:", "Bash"] {
            assert!(
                shown.contains(fragment),
                "long row lost its tail {fragment}"
            );
        }
        // A note one line over the band scrolls one step in either direction.
        picker.key(KeyCode::Down);
        let shown = capture_screen(&mut picker, &mut terminal);
        for fragment in ["Noted", "dropped", "unknown", "tools:", "Apple", "Banana"] {
            assert!(shown.contains(fragment), "noted row lost {fragment}");
        }
        picker.key(KeyCode::PageDown);
        let shown = capture_screen(&mut picker, &mut terminal);
        for fragment in ["Hotel", "Juliet"] {
            assert!(
                shown.contains(fragment),
                "noted row lost its tail {fragment}"
            );
        }
        picker.key(KeyCode::PageUp);
        picker.key(KeyCode::PageUp);
        let shown = capture_screen(&mut picker, &mut terminal);
        assert!(shown.contains("Apple"), "scroll did not return to the head");
        Ok(())
    }

    #[test]
    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    fn empty_roster_hint_reopens_with_escape_then_f7_without_changing_the_draft_or_selection(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut client = Client::new(
            EmptyRoster {
                inner: ContractDouble::new(),
                reads: 0,
                empty: false,
            },
            Bounds::PRODUCTION,
        );
        client.refresh_projects()?;
        let project = client.projects().first().cloned().ok_or("project")?;
        client.select_project(&project)?;
        let agent = client.agents().first().ok_or("agent")?.id.clone();
        client.select_agent(&agent)?;
        let model = client
            .models()
            .iter()
            .find(|row| row.enabled)
            .cloned()
            .ok_or("model")?;
        client.select_model(&model)?;
        let session = client.create_session()?;
        // The real session creation route binds its project and saved choices.
        // Only the subsequent offered roster becomes empty.
        client.peer_mut().empty = true;
        client.resume_session(&session)?;
        assert_eq!(
            client.peer_mut().reads,
            1,
            "establish the empty roster read"
        );
        assert!(client.agents().is_empty());
        assert_eq!(client.selected(), Some(&session));
        let agent = client.agent().cloned();
        let model = client.model().cloned();

        let (tx, commands) = crate::command_channel();
        let updates = crate::backend::Handoff::default();
        let (wake, _ready) = mpsc::sync_channel(1);
        let backend = Backend {
            commands: tx,
            updates,
            wake,
        };
        let mut ui = Ui::new(false);
        ui.launch.update(crate::launch::Readiness::Manual);
        ui.session_update(client.selected().cloned(), false);
        ui.model_update(model.clone(), None, None);
        ui.composer.insert_str("unsent draft");
        ui.composer.move_cursor(ratatui_textarea::CursorMove::Back);
        let cursor = ui.composer.cursor();
        ui.agent_picker = Some(Picker::new(client.agents().to_vec()));

        let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
        terminal.draw(|frame| ui.draw(frame))?;
        assert!(ui
            .agent_picker
            .as_ref()
            .is_some_and(|picker| picker.rows.is_empty()));

        assert!(!keys::handle_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?);
        assert!(ui.agent_picker.is_none());
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(keys::draft(&ui), "unsent draft");
        assert_eq!(ui.composer.cursor(), cursor);

        assert!(!keys::handle_key(
            KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?);
        assert!(matches!(commands.try_recv()?, Command::Roster));
        assert!(matches!(
            commands.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        // Backend's Roster command reads the cached Client roster; it does not
        // retry startup or issue another wire read. Apply its picker snapshot.
        ui.agent_picker = Some(Picker::new(client.agents().to_vec()));
        terminal.draw(|frame| ui.draw(frame))?;
        assert!(ui
            .agent_picker
            .as_ref()
            .ok_or("reopened picker")?
            .selected()
            .is_none());
        assert_eq!(client.peer_mut().reads, 1);
        assert_eq!(client.selected(), Some(&session));
        assert_eq!(client.agent(), agent.as_ref());
        assert_eq!(client.model(), model.as_ref());
        assert_eq!(ui.model, model);
        assert!(!ui.session_update(client.selected().cloned(), false));
        assert!(ui.pending.is_none());
        assert_eq!(keys::draft(&ui), "unsent draft");
        assert_eq!(ui.composer.cursor(), cursor);
        Ok(())
    }

    #[test]
    fn choosing_an_agent_is_explicit_and_does_not_submit_the_draft(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (tx, commands) = crate::command_channel();
        let updates = crate::backend::Handoff::default();
        let (wake, _) = mpsc::sync_channel(1);
        let backend = Backend {
            commands: tx,
            updates,
            wake,
        };
        let mut ui = Ui::new(false);
        ui.composer.insert_str("unsent draft");
        let agent = grapt_client::wire::AgentRow {
            id: grapt_client::wire::agent_id("agt_explicit")?,
            name: "Explicit agent".into(),
            description: None,
            mode: grapt_api::u3a::AgentInfoDtoMode::Primary,
            hidden: false,
            source: None,
        };
        ui.agent_picker = Some(Picker::new(vec![agent.clone()]));
        assert!(!keys::paste("hidden edit", &mut ui));
        keys::handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert!(matches!(commands.try_recv()?, Command::ChooseAgent(id) if id == agent.id));
        keys::handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut ui,
            &backend,
        )?;
        assert!(commands.try_recv().is_err());
        ui.model_update(None, Some(Vec::new()), None);
        assert!(ui.agent_picker.is_none() && ui.models.is_some());
        assert_eq!(keys::draft(&ui), "unsent draft");
        Ok(())
    }

    #[test]
    fn quit_closes_without_sending_a_command_or_prompt() -> Result<(), String> {
        let (tx, commands) = crate::command_channel();
        let updates = crate::backend::Handoff::default();
        let (wake, _) = mpsc::sync_channel(1);
        let backend = Backend {
            commands: tx,
            updates,
            wake,
        };
        let mut ui = Ui::new(false);
        ui.composer.insert_str("/quit");
        assert!(keys::handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut ui,
            &backend
        )?);
        assert!(commands.try_recv().is_err());
        Ok(())
    }
}
