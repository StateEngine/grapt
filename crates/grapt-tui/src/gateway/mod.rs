//! Project → StateEngine agent → active session selector, pending the real API.
//! The example drives this view with typed read fixtures. The native binary
//! does not compile this module or infer availability from environment/config.
pub mod contract;
mod render;
use contract::*;
use crossterm::event::KeyCode;

const MAX_RETAINED: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Connected,
    Disconnected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Projects,
    Agents,
    Sessions,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    ListProjects(PageRequest),
    ListAgents(ProjectId, PageRequest),
    ProjectActiveSessions(ProjectId, PageRequest),
}
#[derive(Debug)]
pub enum Reply {
    Projects(Read<Page<ProjectInfo>>),
    Agents(Read<Page<AgentInfo>>),
    Sessions(Read<Page<ActiveSession>>),
}
#[derive(Debug)]
pub struct Selection {
    pub identity: SessionIdentity,
}

#[derive(Debug)]
pub struct Gateway {
    screen: Screen,
    project: Option<ProjectInfo>,
    agent: Option<AgentInfo>,
    projects: Vec<ProjectInfo>,
    agents: Vec<AgentInfo>,
    sessions: Vec<ActiveSession>,
    query: String,
    selected: usize,
    next: Option<Cursor>,
    status: String,
    sequence: u64,
    pending: Option<(u64, Request)>,
    complete: bool,
    selection: Option<Selection>,
}
impl Gateway {
    /// Only a successful engine capability/availability probe supplies Connected.
    pub fn open(availability: Availability) -> Option<Self> {
        if availability == Availability::Disconnected {
            return None;
        }
        let mut gateway = Self {
            screen: Screen::Projects,
            project: None,
            agent: None,
            projects: Vec::new(),
            agents: Vec::new(),
            sessions: Vec::new(),
            query: String::new(),
            selected: 0,
            next: None,
            status: String::new(),
            sequence: 0,
            pending: None,
            complete: false,
            selection: None,
        };
        gateway.reload();
        Some(gateway)
    }
    pub fn pending(&self) -> Option<(u64, &Request)> {
        self.pending.as_ref().map(|(id, request)| (*id, request))
    }
    pub fn take_selection(&mut self) -> Option<Selection> {
        self.selection.take()
    }
    fn load(&mut self, after: Option<Cursor>) {
        let page = PageRequest {
            after,
            limit: 100,
            max_bytes: 65_536,
        };
        let request = match self.screen {
            Screen::Projects => Request::ListProjects(page),
            Screen::Agents => {
                let Some(project) = &self.project else {
                    return;
                };
                Request::ListAgents(project.id.clone(), page)
            }
            Screen::Sessions => {
                let Some(project) = &self.project else {
                    return;
                };
                Request::ProjectActiveSessions(project.id.clone(), page)
            }
        };
        self.sequence = self.sequence.saturating_add(1);
        self.pending = Some((self.sequence, request));
        self.status = "Loading…".into();
    }
    pub fn reload(&mut self) {
        self.selection = None;
        self.query.clear();
        self.selected = 0;
        self.next = None;
        self.complete = false;
        match self.screen {
            Screen::Projects => self.projects.clear(),
            Screen::Agents => self.agents.clear(),
            Screen::Sessions => self.sessions.clear(),
        }
        self.load(None);
    }
    pub fn accept(&mut self, ticket: u64, reply: Result<Reply, ApiError>) {
        let Some((pending, request)) = &self.pending else {
            return;
        };
        if ticket != *pending {
            return;
        }
        let result = match (request, reply) {
            (_, Err(error)) => Err(format!(
                "{}: {} · F5 reload",
                error.code,
                crate::gateway::render::literal(&error.message)
            )),
            (Request::ListProjects(_), Ok(Reply::Projects(read))) => page(read).and_then(|p| {
                validate(&p, self.projects.len())?;
                if p.items
                    .iter()
                    .any(|row| row.id.0.len() > 256 || row.name.len() > 256)
                {
                    return Err("Invalid project record".into());
                }
                self.next = p.next;
                self.complete = p.window.complete;
                self.projects.extend(p.items);
                Ok(())
            }),
            (Request::ListAgents(project, _), Ok(Reply::Agents(read))) => {
                page(read).and_then(|p| {
                    validate(&p, self.agents.len())?;
                    if p.items.iter().any(|row| {
                        &row.project != project
                            || row.id.0.len() > 256
                            || row.name.len() > 256
                            || row.role.len() > 2048
                    }) {
                        return Err("Agent page does not match the selected project".into());
                    }
                    self.next = p.next;
                    self.complete = p.window.complete;
                    self.agents.extend(p.items);
                    Ok(())
                })
            }
            (Request::ProjectActiveSessions(project, _), Ok(Reply::Sessions(read))) => page(read)
                .and_then(|p| {
                    validate(&p, self.sessions.len())?;
                    if p.items.iter().any(|row| {
                        &row.target.binding.project != project
                            || row.target.binding.session.0.len() > 256
                    }) {
                        return Err("Session page does not match the selected project".into());
                    }
                    self.next = p.next;
                    self.complete = p.window.complete;
                    self.sessions.extend(p.items);
                    Ok(())
                }),
            _ => Err("Unexpected gateway response".into()),
        };
        self.pending = None;
        self.status = match result {
            Err(error) => error,
            Ok(()) if self.next.is_some() => "F8 more · F5 reload".into(),
            Ok(()) if !self.complete => "Results incomplete · F5 reload".into(),
            Ok(()) => "F5 reload".into(),
        };
    }
    fn labels(&self) -> Vec<(usize, String)> {
        let query = self.query.to_lowercase();
        let rows: Vec<_> = match self.screen {
            Screen::Projects => self
                .projects
                .iter()
                .enumerate()
                .map(|(i, p)| (i, format!("{} · {}", p.name, p.root.display())))
                .collect(),
            Screen::Agents => self
                .agents
                .iter()
                .enumerate()
                .map(|(i, a)| (i, format!("{} · {}", a.name, a.role)))
                .collect(),
            Screen::Sessions => self
                .sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    self.agent
                        .as_ref()
                        .is_some_and(|a| a.id == s.target.binding.agent)
                })
                .map(|(i, s)| {
                    (
                        i,
                        format!("{} · {:?}", s.target.binding.session.0, s.activity),
                    )
                })
                .collect(),
        };
        rows.into_iter()
            .map(|(i, s)| (i, render::literal(&s)))
            .filter(|(_, s)| s.to_lowercase().contains(&query))
            .collect()
    }
    /// Returns true when backing out of the project screen. No key mutates an
    /// engine project, agent or binding; selection is a request for the owner.
    pub fn key(&mut self, key: KeyCode) -> bool {
        if key == KeyCode::Esc {
            self.selection = None;
            self.pending = None;
            self.query.clear();
            self.selected = 0;
            self.screen = match self.screen {
                Screen::Projects => return true,
                Screen::Agents => Screen::Projects,
                Screen::Sessions => Screen::Agents,
            };
            self.next = None;
            self.complete = false;
            self.status = "F5 reload".into();
            return false;
        }
        if self.pending.is_some() {
            return false;
        }
        match key {
            KeyCode::F(5) => self.reload(),
            KeyCode::F(8) if self.next.is_some() => self.load(self.next.clone()),
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = self
                    .selected
                    .saturating_add(1)
                    .min(self.labels().len().saturating_sub(1))
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            KeyCode::Char(ch) if !ch.is_control() && self.query.len() + ch.len_utf8() <= 256 => {
                self.query.push(ch);
                self.selected = 0;
            }
            KeyCode::Enter => {
                let Some((index, _)) = self.labels().get(self.selected).cloned() else {
                    return false;
                };
                match self.screen {
                    Screen::Projects => {
                        self.project = self.projects.get(index).cloned();
                        self.agent = None;
                        self.screen = Screen::Agents;
                        self.reload();
                    }
                    Screen::Agents => {
                        self.agent = self.agents.get(index).cloned();
                        self.screen = Screen::Sessions;
                        self.reload();
                    }
                    Screen::Sessions => {
                        self.selection = self.sessions.get(index).map(|s| Selection {
                            identity: s.target.binding.clone(),
                        });
                        self.status = "Selection awaits engine binding".into();
                    }
                }
            }
            _ => {}
        }
        false
    }
    pub fn draw(&self, frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect) {
        render::draw(self, frame, area);
    }
}
fn page<T>(read: Read<Page<T>>) -> Result<Page<T>, String> {
    match read {
        Read::Present { value } => Ok(value),
        Read::Absent { checked } => Err(format!("Unavailable: {}", render::literal(&checked))),
        Read::Unknown { what } => Err(format!("Unknown: {}", render::literal(&what))),
        Read::Unreadable { reason, .. } => {
            Err(format!("Unable to read: {}", render::literal(&reason)))
        }
    }
}
fn validate<T>(page: &Page<T>, retained: usize) -> Result<(), String> {
    if page.items.len() > 100
        || retained.saturating_add(page.items.len()) > MAX_RETAINED
        || page.window.bytes > 65_536
        || page.window.returned != page.items.len() as u64
        || page.next.as_ref().is_some_and(|c| c.0.len() > 256)
    {
        return Err("Gateway page exceeds its bounds; reload or narrow the project".into());
    }
    Ok(())
}
