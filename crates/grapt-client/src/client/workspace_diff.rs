//! One fresh snapshot of the selected backend workspace, never the TUI host.
use super::*;
impl<C: Connection> Client<C> {
    pub fn workspace_diff(&self) -> Result<Option<&crate::wire::WorkspaceDiff>, &str> {
        self.workspace_diff
            .as_ref()
            .map(Option::as_ref)
            .map_err(String::as_str)
    }
    pub fn refresh_workspace_diff(&mut self) {
        self.workspace_diff = self
            .read_workspace_diff()
            .map(Some)
            .map_err(|error| error.to_string());
    }
    fn read_workspace_diff(&mut self) -> Result<crate::wire::WorkspaceDiff, ClientError> {
        let directory = self
            .project
            .as_ref()
            .ok_or(ClientError::NoProject)?
            .worktree
            .clone();
        let bytes = self.ingest(
            Operation::WorkspaceDiff,
            None,
            None,
            None,
            Some(&directory),
            None,
        )?;
        Ok(crate::wire::decode_workspace_diff(&bytes)?)
    }
}
