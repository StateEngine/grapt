//! Application-default authorization remains with the connection owner.
use super::{connection_use::ConnectionUse, vault::VaultSource};
use grapt_provider::{CredentialError, RequestAuthenticationFuture, RequestAuthenticator};
use std::{path::PathBuf, sync::Arc};

pub(super) struct Authenticator {
    root: PathBuf,
    vault: VaultSource,
    session: Arc<grapt_login::vertex::adc::Session>,
    authority: Arc<ConnectionUse>,
}
impl Authenticator {
    pub(super) fn new(
        root: PathBuf,
        vault: VaultSource,
        session: Arc<grapt_login::vertex::adc::Session>,
        authority: Arc<ConnectionUse>,
    ) -> Self {
        Self {
            root,
            vault,
            session,
            authority,
        }
    }
    fn validate(&self) -> Result<(), CredentialError> {
        use grapt_provider::CredentialUseAuthority;
        self.authority.validate()?;
        self.vault
            .check_adc(&self.root)
            .map_err(|_| CredentialError::UseAuthorityUnavailable)
    }
}
impl RequestAuthenticator for Authenticator {
    fn authenticate(&self, request: reqwest::Request) -> RequestAuthenticationFuture<'_> {
        Box::pin(async move {
            self.validate()?;
            let request = self
                .session
                .authorize(request)
                .await
                .map_err(|_| CredentialError::VertexAuthFailed)?;
            self.validate()?;
            Ok(request)
        })
    }
}
