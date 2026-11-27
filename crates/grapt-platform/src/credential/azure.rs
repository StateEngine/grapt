//! Azure authorization remains behind the original connection's use authority.
use super::{connection_use::ConnectionUse, vault::VaultSource};
use grapt_provider::{CredentialError, RequestAuthenticationFuture, RequestAuthenticator};
use std::{path::PathBuf, sync::Arc};

pub(super) struct Authenticator {
    root: PathBuf,
    vault: VaultSource,
    authority: Arc<ConnectionUse>,
}
impl Authenticator {
    pub(super) fn new(root: PathBuf, vault: VaultSource, authority: Arc<ConnectionUse>) -> Self {
        Self {
            root,
            vault,
            authority,
        }
    }
    fn validate(&self) -> Result<(), CredentialError> {
        use grapt_provider::CredentialUseAuthority;
        self.authority.validate()?;
        if !self
            .vault
            .azure_cli(&self.root)
            .map_err(|_| CredentialError::UseAuthorityUnavailable)?
        {
            return Err(CredentialError::Revoked);
        }
        Ok(())
    }
}
impl RequestAuthenticator for Authenticator {
    fn authenticate(&self, request: reqwest::Request) -> RequestAuthenticationFuture<'_> {
        Box::pin(async move {
            self.validate()?;
            let request = grapt_login::azure::authorize(request)
                .await
                .map_err(|_| CredentialError::UseAuthorityUnavailable)?;
            self.validate()?;
            Ok(request)
        })
    }
}
