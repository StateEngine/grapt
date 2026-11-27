//! Keep AWS resolution behind the original connection's credential owner.
use super::{connection_use::ConnectionUse, vault::VaultSource};
use grapt_provider::{CredentialError, RequestAuthenticationFuture, RequestAuthenticator};
use std::{path::PathBuf, sync::Arc};

pub(super) struct Signer {
    root: PathBuf,
    vault: VaultSource,
    selection: grapt_login::aws::Selection,
    authority: Arc<ConnectionUse>,
}
impl Signer {
    pub(super) fn new(
        root: PathBuf,
        vault: VaultSource,
        selection: grapt_login::aws::Selection,
        authority: Arc<ConnectionUse>,
    ) -> Self {
        Self {
            root,
            vault,
            selection,
            authority,
        }
    }
    fn validate(&self) -> Result<(), CredentialError> {
        use grapt_provider::CredentialUseAuthority;
        self.authority.validate()?;
        if self
            .vault
            .aws_selection(&self.root)
            .map_err(|_| CredentialError::UseAuthorityUnavailable)?
            .as_ref()
            != Some(&self.selection)
        {
            return Err(CredentialError::Revoked);
        }
        Ok(())
    }
}
impl RequestAuthenticator for Signer {
    fn authenticate(&self, request: reqwest::Request) -> RequestAuthenticationFuture<'_> {
        Box::pin(async move {
            self.validate()?;
            let request = self
                .selection
                .sign(request, &grapt_store::SystemClock)
                .await
                .map_err(|_| CredentialError::AwsSigningFailed)?;
            self.validate()?;
            Ok(request)
        })
    }
}
