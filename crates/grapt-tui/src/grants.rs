//! A presentation of the existing Client's grant inventory; never authority.
use crossterm::event::KeyCode;
use grapt_api::u3a::{SandboxGrantDto, SandboxGrantScopeDto};
use grapt_client::{wire::SessionWireId, Client, Connection};
use grapt_tui_transcript::palette::Palette;
use ratatui::{
    layout::Rect,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

#[derive(Debug)]
pub struct Snapshot {
    pub session: SessionWireId,
    pub rows: Result<Vec<SandboxGrantDto>, String>,
    pub policy: String,
    pub notice: String,
}
impl Snapshot {
    pub fn read<C: Connection>(client: &mut Client<C>, notice: String) -> Option<Self> {
        client.selected()?;
        // A failed read is recorded, and read back, as the inventory's rows.
        let _ = client.refresh_sandbox_grants();
        Self::of(client, notice)
    }

    /// The grants last read for the selected Session, without a request.
    pub fn of<C: Connection>(client: &Client<C>, notice: String) -> Option<Self> {
        let session = client.selected()?.clone();
        let rows = client
            .sandbox_grants()
            .map(|rows| rows.unwrap_or_default().to_vec())
            .map_err(str::to_owned);
        Some(Self {
            session,
            rows,
            policy: client
                .preset_line()
                .unwrap_or_else(|| "Preset unavailable".into()),
            notice,
        })
    }
}

#[derive(Debug)]
pub struct Request {
    pub session: SessionWireId,
    pub grant: SandboxGrantDto,
}

pub struct Pane {
    snapshot: Snapshot,
    selected: usize,
    scroll: usize,
    page: usize,
    pending: bool,
}
impl Pane {
    pub fn new(snapshot: Snapshot) -> Self {
        Self {
            snapshot,
            selected: 0,
            scroll: 0,
            page: 1,
            pending: false,
        }
    }
    pub fn retry(&mut self) {
        self.pending = false;
    }
    pub fn key(&mut self, key: KeyCode) -> Option<Request> {
        let rows = self.snapshot.rows.as_ref().ok()?;
        match key {
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.scroll = 0;
            }
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(rows.len().saturating_sub(1));
                self.scroll = 0;
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(self.page),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(self.page),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = usize::MAX,
            KeyCode::Enter if !self.pending => {
                let grant = rows.get(self.selected)?.clone();
                self.pending = true;
                return Some(Request {
                    session: self.snapshot.session.clone(),
                    grant,
                });
            }
            _ => {}
        }
        None
    }
    pub fn draw(&mut self, frame: &mut Frame<'_>, mut area: Rect, p: Palette) {
        if area.width < 30 || area.height < 10 {
            area = frame.area();
        }
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Sandbox grants ")
            .style(p.normal());
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let text = match &self.snapshot.rows {
            Err(error) => format!("Grant list unavailable: {error}"),
            Ok(rows) => match rows.get(self.selected) {
                None => "No sandbox grants in this session or project.".into(),
                Some(grant) => {
                    let scope = match grant.scope().get() {
                        SandboxGrantScopeDto::Run => "This run",
                        SandboxGrantScopeDto::Session => "This session",
                        SandboxGrantScopeDto::Project => "Always for this project",
                    };
                    let operation = grant
                        .operation()
                        .as_present()
                        .map(|operation| format!("\nOperation: {}", operation.as_inner()))
                        .unwrap_or_default();
                    format!(
                        "Grant {}/{} · {scope}{operation}\n{}",
                        self.selected + 1,
                        rows.len(),
                        grant.resource().get().as_str()
                    )
                }
            },
        };
        let heading = format!(
            "{}\n{}\n/permissions inspect|author|maintainer changes the preset",
            self.snapshot.policy, self.snapshot.notice
        );
        frame.render_widget(
            Paragraph::new(grapt_tui_transcript::markdown::literal(&heading)),
            Rect {
                height: inner.height.min(3),
                ..inner
            },
        );
        let content = Rect {
            y: inner.y.saturating_add(3),
            height: inner.height.saturating_sub(5),
            ..inner
        };
        let rows = grapt_client::cells::wrap(
            &grapt_tui_transcript::markdown::literal(&text),
            usize::from(content.width.max(1)),
        );
        self.page = usize::from(content.height).max(1);
        self.scroll = self.scroll.min(rows.len().saturating_sub(self.page));
        let end = self.scroll.saturating_add(self.page).min(rows.len());
        frame.render_widget(Paragraph::new(rows[self.scroll..end].join("\n")), content);
        frame.render_widget(
            Paragraph::new(if self.pending {
                "Revocation pending; cleanup must finish before confirmation.\nEsc close"
            } else {
                "↑/↓ select · PgUp/PgDn review\nEnter revoke · Esc close"
            })
            .style(p.normal().fg(p.accent)),
            Rect {
                y: content.bottom(),
                height: inner.height.min(2),
                ..inner
            },
        );
    }
}

#[cfg(test)]
mod tests;
