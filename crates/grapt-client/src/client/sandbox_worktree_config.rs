//! Native user consent targets only the exact read-only preview, once.
use super::*;

#[derive(Debug)]
pub(super) struct Preview {
    session: SessionWireId,
    path: String,
}

impl<C: Connection> Client<C> {
    pub fn preview_sandbox_worktree_config(
        &mut self,
    ) -> Result<crate::wire::SandboxWorktreeConfig, ClientError> {
        // A refused or malformed new preview cannot leave an older target armed.
        self.sandbox_worktree_config_preview = None;
        let session = self.selected.clone().ok_or(ClientError::NoSelection)?;
        let bytes = self.ingest(
            Operation::SessionSandboxWorktreeConfigGet,
            Some(&session),
            None,
            None,
            None,
            None,
        )?;
        let preview = crate::wire::decode_sandbox_worktree_config(&bytes)?;
        if preview.missing {
            if let Some(path) = &preview.path {
                self.sandbox_worktree_config_preview = Some(Preview {
                    session,
                    path: path.clone(),
                });
            }
        }
        Ok(preview)
    }

    /// Call only for explicit native `/sandbox worktree-config create confirm`.
    /// Opening or closing the read-only preview never calls this method.
    pub fn confirm_sandbox_worktree_config_create(
        &mut self,
    ) -> Result<crate::wire::SandboxWorktreeConfig, ClientError> {
        // Consume before any effect: every outcome requires a fresh preview.
        // In particular, an uncertain transport reply never retries this POST.
        let preview =
            self.sandbox_worktree_config_preview
                .take()
                .ok_or(ClientError::InvalidInput(
                    "Preview with /sandbox worktree-config before confirming creation.",
                ))?;
        if self.selected.as_ref() != Some(&preview.session) {
            return Err(ClientError::InvalidInput(
                "Session changed; run /sandbox worktree-config again before confirming.",
            ));
        }
        let body = crate::wire::encode_sandbox_worktree_config_consent(&preview.path)?;
        let bytes = self.ingest(
            Operation::SessionSandboxWorktreeConfigCreate,
            Some(&preview.session),
            None,
            Some(body),
            None,
            None,
        )?;
        Ok(crate::wire::decode_sandbox_worktree_config(&bytes)?)
    }
}
