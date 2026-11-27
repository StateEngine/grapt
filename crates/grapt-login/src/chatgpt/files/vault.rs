//! One destination-bound OAuth snapshot per connection.
use super::*;
use crate::vault::{
    CredentialVault, CredentialView, DestinationBinding, SealedFileVault, VaultError, VaultKey,
    VaultSecret,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VaultState {
    key: VaultKey,
    binding: DestinationBinding,
}
fn refused(error: VaultError) -> SignInError {
    SignInError::Store(error.to_string())
}
impl SignInFiles {
    /// Adopt the explicit bound format for a new connection's file namespace.
    pub fn with_vault(
        mut self,
        connection: grapt_types::ProviderId,
        binding: DestinationBinding,
    ) -> Result<Self, SignInError> {
        if self.is_legacy() {
            return Err(SignInError::Store(
                "vault state needs an independent connection namespace".into(),
            ));
        }
        self.vault = Some(VaultState {
            key: VaultKey::new(connection, &self.state).map_err(refused)?,
            binding,
        });
        Ok(self)
    }
    pub fn is_vaulted(&self) -> bool {
        self.vault.is_some()
    }
    pub(in crate::chatgpt) fn check_refresh(&self, issuer: &str) -> Result<(), SignInError> {
        if let Some(vault) = &self.vault {
            vault.binding.check_refresh(issuer).map_err(refused)?;
        }
        Ok(())
    }
}
impl VaultState {
    pub(super) fn load(&self, profile: &Path) -> Result<Option<Tokens>, SignInError> {
        let Some(secret) = SealedFileVault::open(profile)
            .map_err(refused)?
            .get(&self.key, &self.binding)
            .map_err(refused)?
        else {
            return Ok(None);
        };
        match secret.expose() {
            CredentialView::OAuth {
                access,
                refresh,
                expires_at,
                account,
            } => Ok(Some(Tokens {
                access: access.into(),
                refresh: refresh.into(),
                expires_at,
                account: account.into(),
            })),
            CredentialView::AzureCli
            | CredentialView::VertexAdc
            | CredentialView::AwsChain(_)
            | CredentialView::ApiKey(_)
            | CredentialView::VertexServiceAccount { .. }
            | CredentialView::WatsonxIam { .. } => Err(refused(VaultError::Corrupt)),
        }
    }
    pub(super) fn save(
        &self,
        profile: &Path,
        tokens: &Tokens,
        updated: Timestamp,
    ) -> Result<(), SignInError> {
        SealedFileVault::open(profile)
            .map_err(refused)?
            .put(
                &self.key,
                &self.binding,
                VaultSecret::oauth(tokens.clone()).map_err(refused)?,
                updated,
            )
            .map_err(refused)
    }
}
