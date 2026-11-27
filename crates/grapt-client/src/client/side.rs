//! One ephemeral question shares the existing bounded operator socket.
use super::*;

impl<C: Connection> Client<C> {
    pub fn side_question_pending(&self) -> bool {
        self.side_session.is_some()
    }

    pub fn operator_request_pending(&self) -> bool {
        self.mcp_auth_pending()
            || self.side_question_pending()
            || self.mcp_content_pending()
            || self.mcp_catalog_pending()
            || self.worktree_pending()
            || self.claude_plugin_pending()
    }

    pub fn cancel_operator_request(&mut self) {
        self.cancel_worktree();
        self.cancel_claude_plugin();
        self.connection.cancel_operator();
        self.mcp_auth = None;
        self.mcp_content = None;
        self.side_session = None;
        self.cancel_mcp_catalog();
    }

    /// Begin without blocking the live event and recovery driver.
    pub fn begin_side_question(&mut self, prompt: &str) -> Result<Option<String>, ClientError> {
        // Host-started I/O never coexists with a pending resume.
        self.abandon_resume();
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        if self.pending.contains_key(&session) {
            return Err(ClientError::Unconfirmed);
        }
        if self.operator_request_pending() {
            return Err(ClientError::NotOffered {
                what: "An operator request is already in progress.",
            });
        }
        if prompt.trim().is_empty() || prompt.len() > 65_536 {
            return Err(ClientError::InvalidInput(
                "Use /side <question>, within 65536 bytes",
            ));
        }
        let wire = WireRequest::new(Operation::SideQuestion, Some(&session), None)?
            .with_body(crate::wire::encode_side_question(prompt)?);
        self.side_session = Some(session);
        let result = self
            .connection
            .begin_operator(&wire)
            .map_err(ClientError::from);
        self.side_result(result)
    }

    pub fn poll_side_question(&mut self) -> Result<Option<String>, ClientError> {
        if !self.side_question_pending() {
            return Ok(None);
        }
        let result = self.connection.poll_operator().map_err(ClientError::from);
        self.side_result(result)
    }

    fn side_result(
        &mut self,
        result: Result<Option<WireResponse>, ClientError>,
    ) -> Result<Option<String>, ClientError> {
        if matches!(result, Ok(None)) {
            return Ok(None);
        }
        let session = self.side_session.take().ok_or(ClientError::NoSelection)?;
        if self.selected.as_ref() != Some(&session) {
            return Err(ClientError::NotOffered {
                what: "Session changed; side answer discarded.",
            });
        }
        let response = result?.ok_or(ClientError::NoSelection)?;
        let bytes = self.accept_response(Operation::SideQuestion, response)?;
        Ok(Some(crate::wire::decode_side_answer(&bytes)?))
    }
}
