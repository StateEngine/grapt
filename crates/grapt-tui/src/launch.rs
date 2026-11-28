//! One startup intent.
// Copyright 2025 OpenAI. Modified for Grapt's exact-session client and R editor.
use crate::{
    backend::{Backend, Command},
    keys::draft,
    ui::Ui,
};
use grapt_client::{wire::SessionWireId, Client, ClientError, Connection};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Connecting,
    Paused,
    Unresolved,
    Loading,
    #[default]
    Ready,
    Manual,
}

impl Readiness {
    pub fn can_submit(self) -> bool {
        matches!(self, Self::Ready | Self::Manual)
    }

    pub fn attached(self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Manual | Self::Loading | Self::Unresolved
        )
    }
}

#[derive(Default)]
pub struct Launch {
    pub readiness: Readiness,
    intent: Option<String>,
    painted: bool,
}

impl Launch {
    pub fn start(&mut self, text: Option<String>) {
        self.painted = false;
        self.intent = text;
        self.readiness = Readiness::Connecting;
    }

    pub fn has_intent(&self) -> bool {
        self.intent.is_some()
    }

    pub fn waiting_to_send(&self) -> bool {
        self.readiness == Readiness::Ready && self.intent.is_some()
    }

    pub fn painted(&mut self) {
        self.painted = true;
    }

    pub fn cancel(&mut self) {
        self.intent = None;
    }

    pub fn update(&mut self, readiness: Readiness) {
        self.readiness = readiness;
        if readiness == Readiness::Manual {
            self.cancel();
        }
    }
}

/// Attach the existing Session and apply an explicit effort override once.
/// Retrying never reprovisions a Session, submits a prompt or replays a tool.
pub fn attach<C: Connection>(
    client: &mut Client<C>,
    session: &SessionWireId,
) -> Result<Readiness, ClientError> {
    client.refresh_sessions()?;
    client.attach_launch_session(session)?;
    // Selecting the Session reads its snapshot once the stream proves itself;
    // until then readiness is Loading. Submission still rechecks requests at
    // its existing boundary.
    client.refresh_forms()?;
    Ok(readiness(client))
}

/// Choices load on the shared catalog lane, and an attached Session's open
/// requests arrive with its snapshot; optional Git metadata does not gate input.
pub fn readiness<C: Connection>(client: &Client<C>) -> Readiness {
    if client.catalog_pending() || awaiting_snapshot(client) {
        Readiness::Loading
    } else if client.catalog_validated() && client.ready_to_chat() && !protected(client) {
        Readiness::Ready
    } else {
        Readiness::Manual
    }
}

/// A selected Session whose own snapshot has not installed yet: its pending
/// permissions and forms are not known, so nothing may count as unprotected.
fn awaiting_snapshot<C: Connection>(client: &Client<C>) -> bool {
    client.selected().is_some() && !client.selected_snapshot_installed()
}

pub fn protected<C: Connection>(client: &Client<C>) -> bool {
    client.permissions().iter().any(|ask| ask.live) || !client.forms().is_empty()
}

/// Recheck outstanding requests at the submission boundary. A form settled
/// elsewhere must not leave R holding a stale permanent submission block.
pub fn submit<C: Connection>(client: &mut Client<C>, text: &str) -> Result<(), ClientError> {
    submit_with_origin(client, text, false, &[])
}

pub fn submit_startup<C: Connection>(
    client: &mut Client<C>,
    text: &str,
) -> Result<(), ClientError> {
    submit_with_origin(client, text, true, &[])
}

pub fn submit_files<C: Connection>(
    client: &mut Client<C>,
    text: &str,
    files: &[grapt_api::u3a::PromptInputFileAttachmentDto],
) -> Result<(), ClientError> {
    submit_with_origin(client, text, false, files)
}

fn submit_with_origin<C: Connection>(
    client: &mut Client<C>,
    text: &str,
    startup: bool,
    files: &[grapt_api::u3a::PromptInputFileAttachmentDto],
) -> Result<(), ClientError> {
    if !client.launch_preset_ready()? {
        return Err(ClientError::Preset(
            "Preparing launch permissions; draft kept".into(),
        ));
    }
    client.refresh_permissions()?;
    client.refresh_forms()?;
    if protected(client) {
        return Err(ClientError::SelectionIncomplete);
    }
    if startup {
        client.submit_seat_startup(text)
    } else {
        client.submit_prompt_with_files(text, files)
    }
}

/// Input is drained before this call. Consume intent before enqueueing the
/// existing submission command, including when the bounded command queue refuses.
pub fn finish(ui: &mut Ui, backend: &Backend) -> bool {
    if !ui.launch.painted || ui.launch.readiness != Readiness::Ready || ui.launch.intent.is_none() {
        return false;
    }
    let Some(text) = ui.launch.intent.take() else {
        return false;
    };
    if ui.permission.is_some()
        || ui.picker.is_some()
        || ui.models.is_some()
        || ui.usage_pane.is_some()
        || ui.command_pane.is_some()
        || ui.recall.searching()
        || ui.pending.is_some()
        || ui.browsing
        || !draft(ui).is_empty()
    {
        return false;
    }
    // Prepared input never occupies the human editor; take() prevents every replay.
    match crate::send(backend, Command::StartupPrompt(text.clone())) {
        Ok(()) => {
            ui.pending = Some(text);
            ui.status = "Submitting seat startup prompt".into();
        }
        Err(error) => ui.status = error,
    }
    true
}

#[cfg(test)]
mod tests;
