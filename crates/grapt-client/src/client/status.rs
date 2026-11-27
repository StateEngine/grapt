//! Fresh account metadata; usage remains owned by the session projection.
use super::*;

impl<C: Connection> Client<C> {
    pub fn status_account(&self) -> Result<Option<&str>, &str> {
        self.status_account
            .as_ref()
            .map(|value| value.as_deref())
            .map_err(String::as_str)
    }

    pub fn refresh_status(&mut self) {
        self.request_repository_status();
        self.status_account = self
            .read_status_account()
            .map_err(|error| error.to_string());
    }

    fn read_status_account(&mut self) -> Result<Option<String>, ClientError> {
        if let Some(session) = self.selected.clone() {
            self.refresh_selected_model(&session)?;
        }
        let directory = self
            .project
            .as_ref()
            .ok_or(ClientError::NoProject)?
            .worktree
            .clone();
        let provider = self
            .model
            .as_ref()
            .ok_or(ClientError::SelectionIncomplete)?
            .provider_id
            .clone();
        let bytes = self.ingest(
            Operation::ProviderList,
            None,
            None,
            None,
            Some(&directory),
            None,
        )?;
        Ok(crate::wire::decode_provider_account(&bytes, &provider)?)
    }
}
