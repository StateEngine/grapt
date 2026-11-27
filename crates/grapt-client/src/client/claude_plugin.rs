//! Local installation shares the finite operator transport; events remain live.
use super::*;
use crate::wire::FormLocation;

#[derive(Debug, Default)]
pub(super) struct State {
    location: Option<FormLocation>,
}
impl<C: Connection> Client<C> {
    pub fn claude_plugin_pending(&self) -> bool {
        self.claude_plugin.location.is_some()
    }
    pub fn begin_claude_plugin(
        &mut self,
        request: grapt_api::u3a::ClaudePluginRequestDto,
    ) -> Result<Option<Vec<String>>, ClientError> {
        // Host-started I/O never coexists with a pending resume.
        self.abandon_resume();
        if self.operator_request_pending() {
            return Err(ClientError::InvalidInput(
                "An operator request is already in progress; draft kept.",
            ));
        }
        let location = self.form_location().ok_or(ClientError::NoProject)?;
        let wire = WireRequest::new(Operation::ClaudePlugin, None, None)?
            .with_query("location[directory]", &location.directory)
            .with_body(crate::wire::encode_claude_plugin(&request)?);
        self.claude_plugin.location = Some(location);
        let response = self
            .connection
            .begin_operator(&wire)
            .map_err(ClientError::from);
        self.claude_plugin_result(response)
    }
    pub fn poll_claude_plugin(&mut self) -> Result<Option<Vec<String>>, ClientError> {
        if !self.claude_plugin_pending() {
            return Ok(None);
        }
        let response = self.connection.poll_operator().map_err(ClientError::from);
        self.claude_plugin_result(response)
    }
    fn claude_plugin_result(
        &mut self,
        result: Result<Option<WireResponse>, ClientError>,
    ) -> Result<Option<Vec<String>>, ClientError> {
        if matches!(result, Ok(None)) {
            return Ok(None);
        }
        let location = self.claude_plugin.location.take();
        self.connection.cancel_operator();
        let response = result?.ok_or(ClientError::NoProject)?;
        let bytes = self.accept_response(Operation::ClaudePlugin, response)?;
        let message = crate::wire::decode_claude_plugin(&bytes)?;
        if location != self.form_location() {
            return Err(ClientError::InvalidInput(
                "Plugin operation completed for the previous workspace; reload its commands there.",
            ));
        }
        self.refresh_commands();
        Ok(Some(vec![message]))
    }
    pub(super) fn cancel_claude_plugin(&mut self) {
        self.claude_plugin.location = None;
    }
}
