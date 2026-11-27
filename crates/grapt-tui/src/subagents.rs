//! The persistent family switcher under the composer; event ownership stays in Backend.
//! Behaviour follows Grapt's own frozen TS TUI `subagent-switcher.tsx` at
//! 246f9db23a0471c7baf3778e70d55ad138fd3b90: marker, label, task, model, elapsed
//! and ↓ context tokens per row. This is an independent Rust recreation; no code copied.
use crate::{
    backend::{Agent, Command},
    ui::Ui,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::{
    cells::{cells, cut},
    wire::SessionWireId,
};
use grapt_tui_transcript::{markdown::literal, palette::Palette};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::Modifier,
    text::{Line, Span},
    widgets::{Clear, Paragraph, Widget},
};
use std::{collections::HashMap, time::Duration};
use tokio::time::Instant;

/// Longer families scroll through this many rows instead of dropping children.
const MAX_ROWS: usize = 6;

/// The parent first, then its children in reported spawn order.
pub fn family(agents: &[Agent]) -> Vec<Agent> {
    let Some(current) = agents.iter().find(|agent| agent.current) else {
        return Vec::new();
    };
    let root = current.row.parent.as_ref().unwrap_or(&current.row.id);
    let mut rows: Vec<_> = agents
        .iter()
        .filter(|agent| &agent.row.id == root || agent.row.parent.as_ref() == Some(root))
        .cloned()
        .collect();
    rows.sort_by_key(|agent| {
        (
            &agent.row.id != root,
            agent.row.detail.created_ms.unwrap_or(u64::MAX),
        )
    });
    rows
}

pub fn waiting(status: &str) -> bool {
    status == "waiting for provider slot"
        || status.starts_with("waiting for provider slot (position ")
}

pub fn running(status: &str) -> bool {
    matches!(
        status,
        "running" | "retrying" | "compacting" | "waiting for permission" | "waiting for question"
    ) || waiting(status)
}

pub enum Action {
    View(SessionWireId),
    Interrupt(SessionWireId),
}

/// Presentation state only: timers use `tokio::time::Instant` observations.
#[derive(Default)]
pub struct Switcher {
    started: HashMap<SessionWireId, Instant>,
    finished: HashMap<SessionWireId, Duration>,
    viewed_failures: HashMap<SessionWireId, Option<u64>>,
    now: Option<Instant>,
    pub focus: Option<usize>,
    hover: Option<usize>,
    top: usize,
    area: Rect,
    back: Rect,
    visible: Vec<SessionWireId>,
    stamp: u64,
    /// The focus and viewed session last used to place the window.
    placed: Option<(Option<usize>, Option<SessionWireId>)>,
    pub action: Option<Action>,
}

impl Switcher {
    fn members(&self, agents: &[Agent]) -> Vec<Agent> {
        let members = family(agents);
        let root = members.first().map(|agent| agent.row.id.clone());
        members
            .into_iter()
            .filter(|agent| {
                root.as_ref() == Some(&agent.row.id)
                    || match agent.status.as_str() {
                        "completed" | "succeeded" | "interrupted" => false,
                        "failed" => {
                            self.viewed_failures.get(&agent.row.id)
                                != Some(&agent.row.detail.updated_ms)
                        }
                        _ => true,
                    }
            })
            .collect()
    }

    /// Full and immediate paints reserve the same visible family rows.
    pub fn height(&self, agents: &[Agent], viewport: u16) -> u16 {
        let members = self.members(agents).len();
        if members < 2 {
            return 0;
        }
        (members.min(MAX_ROWS) + 1).min(usize::from(viewport / 4)) as u16
    }

    /// Record when members were first seen running; returns whether paint state changed.
    pub fn observe(&mut self, agents: &[Agent], now: Instant) -> bool {
        let members = family(agents);
        let before = self.started.len();
        self.started
            .retain(|id, _| members.iter().any(|a| &a.row.id == id));
        self.finished
            .retain(|id, _| members.iter().any(|a| &a.row.id == id));
        self.viewed_failures
            .retain(|id, _| agents.iter().any(|a| &a.row.id == id));
        for agent in &members {
            if running(&agent.status) {
                self.viewed_failures.remove(&agent.row.id);
                self.finished.remove(&agent.row.id);
                self.started.entry(agent.row.id.clone()).or_insert(now);
            } else if matches!(
                agent.status.as_str(),
                "completed" | "succeeded" | "failed" | "interrupted"
            ) {
                if agent.current && agent.status == "failed" {
                    self.viewed_failures
                        .insert(agent.row.id.clone(), agent.row.detail.updated_ms);
                }
                if let Some(start) = self.started.remove(&agent.row.id) {
                    self.finished
                        .insert(agent.row.id.clone(), now.saturating_duration_since(start));
                }
            }
        }
        let count = self.members(agents).len();
        if count < 2 {
            self.focus = None;
        }
        self.focus = self.focus.filter(|index| *index < count);
        self.tick(now) || before != self.started.len()
    }

    pub fn tick(&mut self, now: Instant) -> bool {
        self.now = Some(now);
        let stamp = self
            .started
            .values()
            .map(|start| now.saturating_duration_since(*start).as_secs())
            .sum::<u64>()
            + self.started.len() as u64;
        std::mem::replace(&mut self.stamp, stamp) != stamp
    }

    /// Finished rows need no timer; unseen failures remain until explicitly viewed.
    pub fn ticking(&self) -> bool {
        !self.started.is_empty()
    }

    /// Everything that changes switcher pixels besides the agent snapshot.
    pub fn view(&self) -> (Option<usize>, Option<usize>, usize, u64) {
        (self.focus, self.hover, self.top, self.stamp)
    }

    fn elapsed(&self, id: &SessionWireId) -> Option<Duration> {
        self.started
            .get(id)
            .and_then(|start| self.now.map(|now| now.saturating_duration_since(*start)))
            .or_else(|| self.finished.get(id).copied())
    }

    fn hit(&self, column: u16, row: u16) -> Option<usize> {
        let point = Position::new(column, row);
        self.area
            .contains(point)
            .then(|| self.top + usize::from(row - self.area.y))
    }

    /// Hover follows the pointer; returns whether the highlighted row changed.
    pub fn hover(&mut self, column: u16, row: u16) -> bool {
        let hover = self.hit(column, row);
        std::mem::replace(&mut self.hover, hover) != hover
    }

    pub fn scroll(&mut self, column: u16, row: u16, rows: i64) -> bool {
        if self.hit(column, row).is_none() {
            return false;
        }
        self.top = self.top.saturating_add_signed(-rows.signum() as isize);
        true
    }

    /// A press on a row views that member; row 0 is always the parent.
    pub fn click(&mut self, agents: &[Agent], column: u16, row: u16) -> bool {
        if self.back.contains(Position::new(column, row)) {
            if let Some(main) = family(agents).first() {
                self.action = Some(Action::View(main.row.id.clone()));
                self.focus = None;
            }
            return true;
        }
        let Some(index) = self.hit(column, row) else {
            return false;
        };
        if let Some(agent) = self.members(agents).get(index) {
            self.focus = Some(index);
            if let Some((focus, _)) = &mut self.placed {
                *focus = self.focus;
            }
            self.action = Some(Action::View(agent.row.id.clone()));
        }
        true
    }

    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, agents: &[Agent], p: Palette) {
        Clear.render(area, buf);
        let members = self.members(agents);
        let visible: Vec<_> = members.iter().map(|agent| agent.row.id.clone()).collect();
        if self.visible != visible {
            let focused = self.focus.and_then(|index| self.visible.get(index));
            self.focus = focused.and_then(|id| visible.iter().position(|next| next == id));
            self.hover = None;
            self.placed = None;
            self.visible = visible;
        }
        if area.is_empty() {
            self.area = area;
            self.back = Rect::default();
            return;
        }
        let child = agents
            .iter()
            .any(|agent| agent.current && agent.row.parent.is_some());
        self.back = if child {
            Rect { height: 1, ..area }
        } else {
            Rect::default()
        };
        let summary = if child {
            format!(
                "← main · {} agents",
                grapt_client::format::count(members.len().saturating_sub(1) as u64)
            )
        } else {
            format!(
                "← {} agents",
                grapt_client::format::count(members.len().saturating_sub(1) as u64)
            )
        };
        Paragraph::new(summary)
            .style(p.normal().fg(p.muted))
            .render(Rect { height: 1, ..area }, buf);
        let area = Rect {
            y: area.y.saturating_add(1),
            height: area.height.saturating_sub(1),
            ..area
        };
        let rows = usize::from(area.height);
        let current = members.iter().position(|agent| agent.current);
        let anchor = self.focus.or(current).unwrap_or(0);
        // Reposition only when focus or the viewed session changes; the wheel owns the rest.
        let key = (
            self.focus,
            current.map(|index| members[index].row.id.clone()),
        );
        if self.placed.as_ref() != Some(&key) {
            self.top = self.top.min(anchor).max((anchor + 1).saturating_sub(rows));
            self.placed = Some(key);
        }
        self.top = self.top.min(members.len().saturating_sub(rows));
        self.area = area;
        for (offset, agent) in members.iter().skip(self.top).take(rows).enumerate() {
            let index = self.top + offset;
            let line = Rect::new(area.x, area.y + offset as u16, area.width, 1);
            let lit = agent.current || self.hover == Some(index) || self.focus == Some(index);
            self.row(buf, line, agent, lit, p);
        }
    }

    fn row(&self, buf: &mut Buffer, area: Rect, agent: &Agent, lit: bool, p: Palette) {
        let base = p.normal().bg(if lit { p.panel } else { p.background });
        let busy = running(&agent.status);
        let marker = if busy { "● " } else { "○ " };
        let task = literal(&agent.row.title).replace(['\n', '_'], " ");
        let (name, task) = if agent.row.parent.is_none() {
            ("main", "")
        } else {
            task.split_once(' ').unwrap_or((&task, ""))
        };
        let elapsed = self
            .elapsed(&agent.row.id)
            .map(clock)
            .unwrap_or_else(|| "—".into());
        let compact = grapt_client::format::count;
        let context = match (
            &agent.context.error,
            agent.context.used,
            agent.context.capacity,
        ) {
            (Some(error), _, _) => format!("ctx unavailable: {}", literal(error)),
            (None, Some(used), Some(capacity)) => {
                format!("{}/{} ctx", compact(used), compact(capacity))
            }
            (None, Some(used), None) => format!("{} ctx", compact(used)),
            (None, None, Some(capacity)) => format!("—/{} ctx", compact(capacity)),
            _ => "— ctx".into(),
        };
        let right = format!("{elapsed} · {context}");
        let width = usize::from(area.width);
        let right = cut(right.trim(), width / 2);
        let left = width.saturating_sub(cells(&right) + 1);
        let waiting = if waiting(&agent.status) {
            " · waiting"
        } else {
            ""
        };
        let text = cut(&format!("{name}  {task}{waiting}"), left.saturating_sub(2));
        let pad = left.saturating_sub(2 + cells(&text));
        Paragraph::new(Line::from(vec![
            Span::styled(marker, base.fg(if busy { p.text } else { p.muted })),
            Span::styled(
                text,
                if agent.current {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    base
                },
            ),
            Span::styled(" ".repeat(pad + 1), base),
            Span::styled(right, base.fg(p.muted)),
        ]))
        .style(base)
        .render(area, buf);
    }
}

fn clock(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

/// The one action path for keys and mouse clicks.
pub fn perform(ui: &mut Ui, backend: &crate::backend::Backend, action: Action) {
    let result = match action {
        Action::View(id) => ui.select_view(backend, id),
        Action::Interrupt(id) => crate::send(backend, Command::InterruptSession(id)),
    };
    if let Err(error) = result {
        ui.status = error;
    }
}

/// Alt+Down enters the switcher; then Up/Down move, Enter views, Ctrl+D interrupts
/// the targeted running member and Esc returns to main. Other keys leave focus.
pub fn key(key: KeyEvent, ui: &mut Ui, backend: &crate::backend::Backend) -> bool {
    let members = ui.switcher.members(&ui.agents);
    if key.code == KeyCode::Esc
        && family(&ui.agents)
            .iter()
            .any(|agent| agent.current && agent.row.parent.is_some())
    {
        ui.switcher.focus = None;
        if let Some(main) = members.first() {
            perform(ui, backend, Action::View(main.row.id.clone()));
        }
        return true;
    }
    if key.code == KeyCode::Down && key.modifiers.contains(KeyModifiers::ALT) {
        if members.len() > 1 {
            ui.switcher.focus = members.iter().position(|agent| agent.current).or(Some(0));
        }
        return members.len() > 1;
    }
    let Some(index) = ui.switcher.focus else {
        return false;
    };
    let target = members.get(index);
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Up if !control => ui.switcher.focus = Some(index.saturating_sub(1)),
        KeyCode::Down if !control => {
            ui.switcher.focus = Some((index + 1).min(members.len().saturating_sub(1)))
        }
        KeyCode::Enter => {
            if let Some(agent) = target {
                perform(ui, backend, Action::View(agent.row.id.clone()));
            }
        }
        KeyCode::Char('d') if control => {
            if let Some(agent) = target.filter(|a| a.row.parent.is_some() && running(&a.status)) {
                perform(ui, backend, Action::Interrupt(agent.row.id.clone()));
            }
        }
        KeyCode::Esc => {
            ui.switcher.focus = None;
            if let Some(main) = members.first() {
                perform(ui, backend, Action::View(main.row.id.clone()));
            }
        }
        _ => {
            ui.switcher.focus = None;
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests;
