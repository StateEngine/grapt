// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// at bb64b894fd67158be0f874872e6d7bfe717e7368. Modified for the existing DTO catalog.
pub(super) mod expand;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::{Client, Connection};
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Block, Clear, Paragraph},
    Frame,
};
use ratatui_textarea::DataCursor;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    rows: Vec<(String, String)>,
    note: Option<String>,
}
impl Catalog {
    pub fn from_client<C: Connection>(client: &Client<C>) -> Self {
        Self {
            rows: client
                .commands()
                .iter()
                .map(|r| {
                    (
                        crate::actions::command_name(r),
                        r.unavailable.as_ref().map_or_else(
                            || r.description.clone(),
                            |reason| format!("unavailable: {reason}"),
                        ),
                    )
                })
                .collect(),
            note: client.commands_status().map(str::to_owned),
        }
    }
}
#[derive(Default)]
pub struct Completion {
    catalog: Catalog,
    query: Option<String>,
    dismissed: Option<String>,
    matches: Vec<(String, String)>,
    selected: usize,
    enabled: bool,
    requested: bool,
}
pub(crate) const BUILTINS: &[(&str, &str)] = &[
    ("palette", "Search native actions and commands"),
    ("settings", "Search and save native display preferences"),
    ("help", "Show shortcuts"),
    ("connect", "Connect or manage providers"),
    ("attach", "Attach an image by path"),
    ("commands", "List/reload workspace commands"),
    ("status", "Session status and usage"),
    ("model", "Choose model and effort"),
    ("agents", "Choose an offered agent"),
    ("effort", "Choose or inspect reasoning effort"),
    ("permissions", "Inspect or change session permissions"),
    (
        "sandbox",
        "Preview worktree-config; create confirm consents to the exact preview",
    ),
    ("new", "Start a session"),
    ("clear", "Start fresh; keep saved history"),
    ("resume", "Choose a saved session"),
    ("rename", "Rename this session"),
    (
        "export",
        "Save session [markdown|json] [path]; never overwrite",
    ),
    ("undo", "Review the last file effect"),
    ("rewind", "Reopen a prompt for editing"),
    ("fork", "Branch here or before an earlier prompt"),
    ("side", "Ask a side question without saving a turn"),
    (
        "worktree",
        "Create, merge, safely remove, or open an isolated checkout",
    ),
    ("quit", "Close the terminal"),
    ("diff", "Inspect workspace changes"),
    ("context", "Inspect last-captured instructions"),
    ("init", "Create repository guidance"),
    (
        "import",
        "Preview Claude or Codex setup; /import apply imports it",
    ),
    (
        "plugin",
        "Add a local marketplace or install a Claude plugin",
    ),
    ("mcp", "Inspect MCP servers or sign in"),
    ("compact", "Compact context"),
    ("reconcile", "Reconcile lost tool runs"),
    ("review", "Review changes [commit|branch|pr]"),
];
pub fn builtin(name: &str) -> bool {
    name == "mcps"
        || BUILTINS.iter().any(|(known, _)| name == *known)
        || crate::actions::ALIASES
            .iter()
            .any(|(alias, _)| name == *alias)
}
impl Completion {
    pub fn commands(&self) -> impl Iterator<Item = (&str, &str)> {
        self.catalog
            .rows
            .iter()
            .map(|(name, description)| (name.as_str(), description.as_str()))
    }
    /// Ordinary drafts do not need the workspace command catalog. Request it
    /// once each time slash input opens, including paste and recall. This
    /// also rescans files when native filesystem watching is unavailable.
    pub fn request_catalog(
        &mut self,
        text: &str,
        backend: &crate::backend::Backend,
    ) -> Result<(), String> {
        let text = text.trim_start();
        let opening = text.starts_with('/')
            && !text.starts_with("//")
            && self.dismissed.as_deref() != text.strip_prefix('/');
        if !opening {
            self.requested = false;
        }
        if !self.requested && opening {
            crate::send(backend, crate::backend::Command::Commands)?;
            self.requested = true;
        }
        Ok(())
    }

    pub fn catalog(&mut self, catalog: Catalog) -> bool {
        if self.catalog == catalog {
            return false;
        }
        self.catalog = catalog;
        self.refilter();
        true
    }
    pub fn sync(&mut self, text: &str, cursor: DataCursor, enabled: bool) {
        let token = text.strip_prefix('/').filter(|q| {
            q.len() <= 512
                && !q.chars().any(char::is_whitespace)
                && cursor == (0, text.chars().count())
        });
        if self.dismissed.as_deref() != token {
            self.dismissed = None;
        }
        let query = token.map(str::to_owned);
        if self.query != query {
            self.selected = 0;
            self.query = query;
            self.refilter();
        }
        self.enabled = enabled;
    }
    fn refilter(&mut self) {
        self.matches.clear();
        let Some(query) = &self.query else {
            return;
        };
        for (name, description) in BUILTINS
            .iter()
            .copied()
            .chain(
                crate::actions::ALIASES
                    .iter()
                    .map(|(alias, target)| (*alias, *target)),
            )
            .chain(
                self.catalog
                    .rows
                    .iter()
                    .map(|(n, d)| (n.as_str(), d.as_str())),
            )
        {
            if name.starts_with(query)
                && !self.matches.iter().any(|(n, _)| n == name)
                && self.matches.len() < 64
            {
                let description = crate::actions::ALIASES
                    .iter()
                    .find(|(alias, _)| *alias == name)
                    .map(|(_, target)| format!("Alias for /{target}"))
                    .unwrap_or_else(|| description.into());
                self.matches.push((name.into(), description));
            }
        }
        self.selected = self.selected.min(self.matches.len().saturating_sub(1));
    }
    pub fn visible(&self) -> bool {
        self.enabled && self.query.is_some() && self.dismissed != self.query
    }
    /// Some means the popup consumed this key; only a suffix insertion edits text.
    pub fn key(&mut self, key: KeyEvent) -> Option<String> {
        if !self.visible()
            || key.code == KeyCode::Enter && !key.modifiers.is_empty()
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return None;
        }
        match key.code {
            KeyCode::Esc => self.dismissed = self.query.clone(),
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.matches.len().saturating_sub(1))
            }
            KeyCode::Tab | KeyCode::Enter => {
                let (name, _) = self.matches.get(self.selected)?;
                let query = self.query.as_deref()?;
                if key.code == KeyCode::Enter && name == query {
                    return None;
                }
                return Some(format!("{} ", name.strip_prefix(query)?));
            }
            _ => return None,
        }
        Some(String::new())
    }
    pub fn draw(
        &self,
        frame: &mut Frame<'_>,
        composer: Rect,
        palette: grapt_tui_transcript::palette::Palette,
    ) {
        if !self.visible() || composer.y == 0 {
            return;
        }
        let height = (self.matches.len().clamp(1, 6) as u16 + 3).min(composer.y);
        let area = Rect {
            x: composer.x,
            y: composer.y - height,
            width: composer.width,
            height,
        };
        let bordered = area.height >= 3;
        let header = area.height >= 4;
        let page = usize::from(area.height) - usize::from(bordered) * 2 - usize::from(header);
        let mut lines = Vec::new();
        if header {
            lines.push(Line::raw(" / commands · Tab complete · Esc close"));
        }
        if self.matches.is_empty() {
            lines.push(Line::raw(grapt_tui_transcript::markdown::literal(
                self.catalog
                    .note
                    .as_deref()
                    .unwrap_or("No matching workspace command; /commands reloads"),
            )));
        } else {
            let start = self.selected.saturating_sub(page.saturating_sub(1));
            lines.extend(self.matches.iter().enumerate().skip(start).take(page).map(
                |(i, (name, desc))| {
                    Line::raw(grapt_tui_transcript::markdown::literal(&format!(
                        " {} /{name}  {desc}",
                        if i == self.selected { "›" } else { " " }
                    )))
                },
            ));
        }
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(lines)
                .style(palette.normal().bg(palette.panel))
                .block(if bordered {
                    Block::bordered()
                } else {
                    Block::default()
                }),
            area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_inserts_only_suffix_and_exact_enter_executes_normally() {
        let mut popup = Completion::default();
        popup.catalog(Catalog {
            rows: vec![("review".into(), "Review changes".into())],
            note: None,
        });
        popup.sync("/rev", DataCursor(0, 4), true);
        assert_eq!(
            popup.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            Some("iew ".into())
        );
        popup.sync("/review", DataCursor(0, 7), true);
        assert_eq!(
            popup.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            None
        );
        assert!(popup
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .is_some());
        popup.sync("/review", DataCursor(0, 7), false);
        popup.sync("/review", DataCursor(0, 7), true);
        assert!(
            !popup.visible(),
            "temporary modal visibility must preserve dismissal"
        );
        popup.sync("/revie", DataCursor(0, 6), true);
        assert!(popup.visible());
        popup.sync("/exp", DataCursor(0, 4), true);
        assert!(builtin("export"));
        assert_eq!(
            popup.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
            Some("ort ".into())
        );
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;
    use std::sync::mpsc;
    #[test]
    fn workspace_commands_wait_for_slash_input_and_pass_a_command_burst() -> Result<(), String> {
        let (commands, receiver) = crate::command_channel();
        let updates = crate::backend::Handoff::default();
        let (wake, _) = mpsc::sync_channel(1);
        let backend = crate::backend::Backend {
            commands,
            updates,
            wake,
        };
        let mut popup = Completion::default();
        for text in ["", "ordinary draft", "//literal", "  //literal"] {
            popup.request_catalog(text, &backend)?;
            assert!(receiver.try_recv().is_err());
        }
        crate::burst(&backend)?;
        popup.request_catalog("/", &backend)?;
        let rest = crate::after_burst(&receiver)?;
        assert!(matches!(rest[..], [crate::backend::Command::Commands]));
        popup.request_catalog("", &backend)?;
        popup.request_catalog("  /review changes", &backend)?;
        assert!(matches!(
            receiver.try_recv(),
            Ok(crate::backend::Command::Commands)
        ));
        for text in ["/review", "/commands", "/other"] {
            popup.request_catalog(text, &backend)?;
            assert!(
                receiver.try_recv().is_err(),
                "request once while completion stays open"
            );
        }
        popup.request_catalog("", &backend)?;
        popup.request_catalog("/", &backend)?;
        assert!(matches!(
            receiver.try_recv(),
            Ok(crate::backend::Command::Commands)
        ));
        popup.sync("/", DataCursor(0, 1), true);
        assert!(popup
            .key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .is_some());
        popup.request_catalog("/", &backend)?;
        assert!(receiver.try_recv().is_err());
        popup.request_catalog("/h", &backend)?;
        assert!(matches!(
            receiver.try_recv(),
            Ok(crate::backend::Command::Commands)
        ));
        Ok(())
    }
    #[test]
    fn popup_navigation_precedes_prompt_recall_and_inserts_one_suffix() -> Result<(), String> {
        let (commands, receiver) = crate::command_channel();
        let updates = crate::backend::Handoff::default();
        let (wake, _) = mpsc::sync_channel(1);
        let backend = crate::backend::Backend {
            commands,
            updates,
            wake,
        };
        let mut ui = crate::ui::Ui::new(false);
        ui.composer.insert_str("/");
        for code in [KeyCode::Down, KeyCode::Up, KeyCode::Down, KeyCode::Tab] {
            crate::keys::handle_key(KeyEvent::new(code, KeyModifiers::NONE), &mut ui, &backend)?;
        }
        let completed = ui.composer.lines().join("\n");
        assert!(BUILTINS
            .iter()
            .any(|(name, _)| completed == format!("/{name} ")));
        assert!(receiver.try_recv().is_err(), "completion must not submit");
        assert!(ui.composer.undo());
        assert_eq!(ui.composer.lines(), &["/"]);
        Ok(())
    }
}

#[cfg(test)]
mod viewport_tests {
    use super::*;
    #[test]
    fn selected_command_stays_visible_in_short_popups() -> Result<(), std::convert::Infallible> {
        for top in [1, 2, 3, 6] {
            let mut popup = Completion::default();
            popup.sync("/", DataCursor(0, 1), true);
            let target = BUILTINS
                .iter()
                .position(|(name, _)| *name == "resume")
                .unwrap_or_else(|| panic!("resume command missing"));
            for _ in 0..target {
                popup.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            }
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(50, 10))?;
            terminal.draw(|frame| {
                popup.draw(
                    frame,
                    Rect {
                        x: 0,
                        y: top,
                        width: 50,
                        height: 3,
                    },
                    grapt_tui_transcript::palette::Palette::new(false),
                )
            })?;
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(
                text.contains("› /resume"),
                "selected command hidden at popup height {top}: {text}"
            );
        }
        Ok(())
    }
    #[test]
    fn modified_enter_remains_a_draft_newline() -> Result<(), String> {
        let (commands, _) = crate::command_channel();
        let updates = crate::backend::Handoff::default();
        let (wake, _) = std::sync::mpsc::sync_channel(1);
        let backend = crate::backend::Backend {
            commands,
            updates,
            wake,
        };
        let mut ui = crate::ui::Ui::new(false);
        ui.composer.insert_str("/he");
        crate::keys::handle_key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT),
            &mut ui,
            &backend,
        )?;
        assert_eq!(ui.composer.lines(), &["/he", ""]);
        Ok(())
    }
}
