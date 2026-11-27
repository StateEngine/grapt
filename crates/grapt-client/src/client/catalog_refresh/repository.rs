//! Optional Git diagnosis follows validated Session choices on the catalog lane.
use super::*;

#[derive(Debug)]
pub(super) struct Read {
    project: crate::wire::ProjectRow,
    pending: bool,
}
impl Read {
    pub(super) fn new(project: crate::wire::ProjectRow) -> Self {
        Self {
            project,
            pending: false,
        }
    }

    pub(super) fn pending(&self) -> bool {
        self.pending
    }
}
impl<C: Connection> Client<C> {
    pub fn repository_status(&self) -> Option<&str> {
        self.catalog_refresh.repository_note.as_deref()
    }

    /// Optional Git diagnosis runs only when the user requests status.
    /// Saved Session binding and live catalogs are already validated without it.
    pub(in crate::client) fn request_repository_status(&mut self) {
        if self.catalog_refresh.repository.is_some() {
            return;
        }
        self.catalog_refresh.repository = self
            .catalog_refresh
            .binding
            .as_ref()
            .filter(|binding| self.project.as_ref() == Some(&binding.project))
            .map(|binding| Read::new(binding.project.clone()));
    }

    pub(super) fn advance_repository(&mut self) -> bool {
        // A queued status read waits for the current catalog transport owner.
        if self.command_catalog.in_flight() {
            return false;
        }
        let Some(mut read) = self.catalog_refresh.repository.take() else {
            return false;
        };
        if self.project.as_ref() != Some(&read.project) {
            self.connection.cancel_catalog();
            return false;
        }
        let result = (|| -> Result<Option<Vec<u8>>, ClientError> {
            let response = if read.pending {
                self.connection.poll_catalog()?
            } else {
                // No Session coordinate: this is an optional Git read, never
                // the source of the already validated Session binding.
                let request = WireRequest::new(Operation::ProjectCurrent, None, None)?
                    .with_query("location[directory]", &read.project.worktree);
                self.connection.begin_catalog(&request)?
            };
            response
                .map(|response| self.accept_response(Operation::ProjectCurrent, response))
                .transpose()
        })();
        let failure = match result {
            Ok(None) => {
                read.pending = true;
                self.catalog_refresh.repository = Some(read);
                return false;
            }
            Ok(Some(bytes)) => match crate::wire::decode_project_current(&bytes) {
                Ok((id, worktree))
                    if id == read.project.id && worktree == read.project.worktree =>
                {
                    None
                }
                Ok(_) => Some("repository differs from the saved Session".to_owned()),
                Err(error) => Some(error.to_string()),
            },
            Err(error) => {
                self.connection.cancel_catalog();
                Some(match error {
                    ClientError::Refused { message, .. } => message,
                    other => other.to_string(),
                })
            }
        };
        self.catalog_refresh.repository_note = failure.map(|reason| {
            let bounded: String = reason
                .chars()
                .filter(|c| !c.is_control())
                .take(512)
                .collect();
            format!("Repository metadata unavailable: {bounded}")
        });
        true
    }
}
