use super::*;
use crate::{
    backend::{Backend, Command},
    send,
    ui::Ui,
};
use crossterm::event::{KeyEvent, KeyModifiers};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Rename { id: SessionWireId, title: String },
    Delete { id: SessionWireId },
}
#[derive(Debug)]
pub struct Search {
    pub query: String,
    pub result: Result<Vec<Agent>, String>,
}
#[derive(Debug)]
pub enum Message {
    Search(Search),
    Unavailable {
        id: SessionWireId,
    },
    Managed {
        request: Request,
        result: Result<(), String>,
        refresh_error: Option<String>,
    },
}

pub fn poll_search(ui: &mut Ui, backend: &Backend, now: Duration) -> bool {
    let Some(picker) = &mut ui.picker else {
        return false;
    };
    let Some(query) = picker.search_due(now) else {
        return false;
    };
    match send(backend, Command::SessionSearch(query.clone())) {
        Ok(()) => picker.requested = Some(query),
        Err(error) => {
            picker.changed = now;
            ui.status = error;
            return true;
        }
    }
    false
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub fn handle(
    key: KeyEvent,
    ui: &mut Ui,
    backend: &Backend,
    now: Duration,
) -> Result<bool, String> {
    let Some(mut picker) = ui.picker.take() else {
        return Ok(false);
    };
    if picker.pending.is_some() {
        if key.code != KeyCode::Esc {
            ui.picker = Some(picker);
        }
        return Ok(false);
    }
    if key.code == KeyCode::Esc {
        if picker.edit.take().is_some() {
            ui.picker = Some(picker);
        }
        return Ok(false);
    }
    if picker.edit.is_some() {
        if key.code == KeyCode::Enter
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
        {
            let request = match &picker.edit {
                Some(Edit::Rename { id, input }) if !input.lines()[0].trim().is_empty() => {
                    Some(Request::Rename {
                        id: id.clone(),
                        title: input.lines()[0].trim().to_owned(),
                    })
                }
                Some(Edit::Delete { id }) => Some(Request::Delete { id: id.clone() }),
                _ => None,
            };
            if let Some(request) = request {
                match send(backend, Command::ManageSession(request.clone())) {
                    Ok(()) => picker.pending = Some(request),
                    Err(error) => ui.status = error,
                }
            } else {
                ui.status = "Enter a session title".into();
            }
        } else if let Some(Edit::Rename { input, .. }) = &mut picker.edit {
            if key.code != KeyCode::Enter {
                let mut edited = input.clone();
                edited.input(key);
                if edited.lines().len() == 1
                    && edited.lines()[0].chars().count() <= 100
                    && !edited.lines()[0].chars().any(char::is_control)
                {
                    *input = edited;
                }
            }
        }
        ui.picker = Some(picker);
        return Ok(false);
    }
    let selected = picker.selected(&ui.agents, &ui.session_pins).cloned();
    let previous_query = picker.query.clone();
    match key.code {
        KeyCode::Char('b') if key.modifiers == KeyModifiers::CONTROL => {
            if let Some(id) = selected {
                if let Err(error) = ui.session_pins.toggle(id) {
                    ui.status = error;
                }
            }
        }
        KeyCode::Char('g') if key.modifiers == KeyModifiers::CONTROL => {
            picker.group = match picker.group {
                Group::Date => Group::Directory,
                Group::Directory => Group::Date,
            };
            picker.offset = 0;
        }
        KeyCode::Char('r' | 'd') if key.modifiers == KeyModifiers::CONTROL => {
            if let Some(id) = selected {
                let row = picker
                    .entries(&ui.agents, &ui.session_pins)
                    .into_iter()
                    .find(|a| a.row.id == id);
                if key.code == KeyCode::Char('r') {
                    if let Some(row) = row {
                        let mut input = TextArea::new(vec![row.row.title.clone()]);
                        input.select_all();
                        picker.edit = Some(Edit::Rename {
                            id,
                            input: Box::new(input),
                        });
                    }
                } else {
                    picker.edit = Some(Edit::Delete { id });
                }
            }
        }
        KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
            picker.clear_filter(&ui.agents, &ui.session_pins)
        }
        KeyCode::Enter => {
            if let Some(id) = selected {
                match ui.select_view(backend, id) {
                    Ok(()) => {
                        ui.navigation.reset();
                        return Ok(false);
                    }
                    Err(error) => ui.status = error,
                }
            } else {
                ui.status = "Selected session is no longer available".into();
            }
        }
        _ if !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            picker.key(key.code, &ui.agents, &ui.session_pins)
        }
        _ => {}
    }
    if picker.query != previous_query {
        picker.schedule(now);
    }
    ui.picker = Some(picker);
    Ok(false)
}

impl Ui {
    pub fn session_message(&mut self, message: Message) {
        match message {
            Message::Unavailable { id } => {
                self.agents.retain(|row| row.row.id != id);
                self.views.forget(&id);
                if let Some(picker) = &mut self.picker {
                    if let Some(Search {
                        result: Ok(rows), ..
                    }) = &mut picker.search
                    {
                        rows.retain(|row| row.row.id != id);
                    }
                    picker.filtered(&self.agents, &self.session_pins);
                }
                self.status = match self.session_pins.remove(&id) {
                    Ok(()) => "Session no longer available".into(),
                    Err(error) => format!("Session no longer available; {error}"),
                };
            }
            Message::Search(search) => {
                if let Some(picker) = &mut self.picker {
                    picker.accept_search(search, &self.agents, &self.session_pins);
                }
            }
            Message::Managed {
                request,
                result,
                refresh_error,
            } => {
                if let (Request::Delete { id }, Ok(())) = (&request, &result) {
                    self.agents.retain(|row| &row.row.id != id);
                }
                if let Some(picker) = &mut self.picker {
                    if picker.pending.as_ref() == Some(&request) {
                        picker.pending = None;
                        if result.is_ok() {
                            picker.edit = None;
                        }
                    }
                    // A successful deletion cannot remain in a retained search page.
                    if result.is_ok() {
                        if let Some(Search {
                            result: Ok(rows), ..
                        }) = &mut picker.search
                        {
                            match &request {
                                Request::Delete { id } => rows.retain(|a| &a.row.id != id),
                                Request::Rename { .. } => {}
                            }
                        }
                        if matches!(request, Request::Rename { .. }) {
                            picker.search = None;
                            picker.requested = None;
                        }
                        picker.filtered(&self.agents, &self.session_pins);
                    }
                }
                self.status = match result {
                    Ok(()) => match &request {
                        Request::Rename { .. } => "Session renamed".into(),
                        Request::Delete { id } => {
                            self.views.forget(id);
                            match self.session_pins.remove(id) {
                                Ok(()) => "Session deleted".into(),
                                Err(error) => format!("Session deleted; {error}"),
                            }
                        }
                    },
                    Err(error) => format!(
                        "Session unchanged or unconfirmed: {}",
                        grapt_tui_transcript::markdown::literal(&error)
                    ),
                };
                if let Some(error) = refresh_error {
                    self.status.push_str(&format!(
                        "; list refresh failed: {}",
                        grapt_tui_transcript::markdown::literal(&error)
                    ));
                }
            }
        }
    }
}
