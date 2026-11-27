//! Delay a cached new conversation's write until the user sends its draft.
use super::*;

#[derive(Debug, Default)]
pub(super) enum Draft {
    #[default]
    Inactive,
    Ready,
    Unconfirmed,
}

impl<C: Connection> Client<C> {
    /// Start a NEW session at an explicit backend directory on the independent
    /// operator lane. Poll with `poll_worktree`; never relocate a saved session.
    pub fn create_session_in(
        &mut self,
        directory: &str,
    ) -> Result<Option<Vec<String>>, ClientError> {
        if directory.is_empty() || directory.len() > 4096 || directory.chars().any(char::is_control)
        {
            return Err(ClientError::InvalidInput(
                "Use /worktree open <absolute backend checkout path>, within 4096 bytes.",
            ));
        }
        // The backend may run on another OS: validate spelling only, never
        // canonicalize or join against the TUI host's current directory.
        if !absolute_backend_directory(directory) {
            return Err(ClientError::InvalidInput(
                "/worktree open requires an absolute backend path; use the path printed by create.",
            ));
        }
        let body = self.session_create_body_in(directory)?;
        self.begin_worktree_request(Operation::SessionCreate, body, None)
    }

    pub(super) fn session_create_body_in(&self, directory: &str) -> Result<Vec<u8>, ClientError> {
        if !self.ready_to_chat() {
            return Err(ClientError::SelectionIncomplete);
        }
        let (Some(agent), Some(model)) = (self.agent.as_ref(), self.model.as_ref()) else {
            return Err(ClientError::SelectionIncomplete);
        };
        Ok(crate::wire::encode_session_create_choice(
            agent,
            &model.id,
            &model.provider_id,
            directory,
            model.variant.as_ref(),
        )?)
    }

    pub(super) fn select_created_session(
        &mut self,
        bytes: &[u8],
    ) -> Result<SessionWireId, ClientError> {
        let id = crate::wire::decode_session(bytes)?.id;
        self.refresh_sessions()?;
        // Creation already saved the exact current model and effort atomically.
        let effort = self.launch_effort.take();
        let result = self.select(&id);
        self.launch_effort = effort;
        result?;
        Ok(id)
    }

    pub fn create_draft_session(&mut self) -> Result<(), String> {
        if self.selected.is_some() || matches!(self.draft_creation, Draft::Inactive) {
            return Ok(());
        }
        if matches!(self.draft_creation, Draft::Unconfirmed) {
            return Err("Session creation is unconfirmed. Draft kept; open Sessions to choose it before sending.".into());
        }
        if !self.ready_to_chat() {
            return Err("Draft kept. Choose an agent and model after the catalog loads.".into());
        }
        // Resolve an explicit launch override against the cached offers before
        // encoding the creation. An unsupported choice keeps the draft and may
        // become available when the background catalog arrives.
        self.apply_launch_effort()
            .map_err(|error| error.to_string())?;
        self.create_session()
            .map_err(|error| format!("Draft kept; session creation could not finish: {error}"))?;
        self.draft_creation = Draft::Inactive;
        Ok(())
    }

    pub(super) fn create_request(&mut self, body: Vec<u8>) -> Result<Vec<u8>, ClientError> {
        if self.worktree.creation_uncertain {
            return Err(ClientError::Worktree(self.worktree_uncertainty()));
        }
        let draft = matches!(self.draft_creation, Draft::Ready);
        if draft {
            self.draft_creation = Draft::Unconfirmed;
        }
        let result = self.ingest(Operation::SessionCreate, None, None, Some(body), None, None);
        // This classification belongs to the creation request itself. A failed
        // list or attachment AFTER a successful create cannot make it unsent.
        if draft && result.as_ref().is_err_and(definitely_unsent) {
            self.draft_creation = Draft::Ready;
        }
        result
    }
}

pub(super) fn absolute_backend_directory(directory: &str) -> bool {
    let bytes = directory.as_bytes();
    directory.starts_with('/')
        || directory.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

fn definitely_unsent(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Transport(
            TransportError::NotConnected
                | TransportError::Unavailable { .. }
                | TransportError::Io { .. }
                | TransportError::Contract(_)
        )
    )
}
