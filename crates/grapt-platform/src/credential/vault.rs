//! Consume only the format and binding captured by the connection grant.
use super::*;
use grapt_login::vault::{
    CredentialVault, CredentialView, DestinationBinding, SealedFileVault, VaultError, VaultKey,
};

#[derive(Clone)]
pub(super) struct VaultSource {
    key: VaultKey,
    binding: DestinationBinding,
    oauth_account: Option<AccountId>,
    kind: Kind,
    files: Option<grapt_login::chatgpt::SignInFiles>,
    messages: Option<grapt_login::vault::MessagesDestination>,
    cloudflare: Option<grapt_types::cloudflare::CloudflareGatewayScope>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Static,
    Aws,
    AzureCli,
    VertexAdc,
    Vertex,
    Watsonx,
}
impl VaultSource {
    pub(super) fn api_origin(&self) -> &str {
        self.binding.api_origin()
    }

    pub(super) fn new(grant: &CredentialGrant) -> Result<Option<Self>, CredentialRefusal> {
        let Some(binding) = grant.credential_format().binding() else {
            return Ok(None);
        };
        let (file, oauth_account) = match grant.chatgpt_files() {
            Some(files) if files.is_vaulted() => {
                (files.state_file(), Some(grant.account().clone()))
            }
            Some(_) => return Err(CredentialRefusal::Vault(VaultError::BindingMismatch)),
            None => (grant.source().as_str(), None),
        };
        Ok(Some(Self {
            kind: if matches!(
                grant.credential_format(),
                grapt_connections::store::CredentialFormat::GoogleAdc { .. }
            ) {
                if !matches!(
                    grant.protocol_family(),
                    ProtocolFamily::VertexGemini | ProtocolFamily::VertexAnthropic
                ) {
                    return Err(CredentialRefusal::GrantStorageMismatch);
                }
                Kind::VertexAdc
            } else if matches!(
                grant.credential_format(),
                grapt_connections::store::CredentialFormat::AwsChain { .. }
            ) {
                if grant.protocol_family() != ProtocolFamily::BedrockConverse {
                    return Err(CredentialRefusal::GrantStorageMismatch);
                }
                Kind::Aws
            } else if matches!(
                grant.credential_format(),
                grapt_connections::store::CredentialFormat::AzureCli { .. }
            ) {
                if grant.protocol_family() != ProtocolFamily::OpenAiResponses {
                    return Err(CredentialRefusal::GrantStorageMismatch);
                }
                Kind::AzureCli
            } else {
                match grant.protocol_family() {
                    ProtocolFamily::VertexGemini | ProtocolFamily::VertexAnthropic => Kind::Vertex,
                    ProtocolFamily::WatsonxChat => Kind::Watsonx,
                    _ => Kind::Static,
                }
            },
            key: VaultKey::new(grant.provider().clone(), file).map_err(CredentialRefusal::Vault)?,
            binding: binding.clone(),
            oauth_account,
            files: grant.chatgpt_files().cloned(),
            messages: match grant.credential_format() {
                grapt_connections::store::CredentialFormat::MessagesBearer {
                    destination, ..
                } => Some(destination.clone()),
                _ => None,
            },
            cloudflare: grant.credential_format().cloudflare().cloned(),
        }))
    }
    pub(super) fn is_static_key(&self) -> bool {
        self.kind == Kind::Static
    }
    pub(super) fn capture(&self, root: &Path) -> Result<String, CredentialRefusal> {
        let check_generation = || {
            self.files
                .as_ref()
                .map(|files| files.check_generation(root))
                .transpose()
                .map(|_| ())
                .map_err(|_| CredentialRefusal::Vault(VaultError::BindingMismatch))
        };
        check_generation()?;
        let secret = SealedFileVault::open(root)
            .map_err(CredentialRefusal::Vault)?
            .get(&self.key, &self.binding)
            .map_err(CredentialRefusal::Vault)?
            .ok_or(CredentialRefusal::Absent)?;
        check_generation()?;
        match (secret.expose(), &self.oauth_account) {
            (CredentialView::WatsonxIam { access, .. }, None) if self.kind == Kind::Watsonx => {
                Ok(access.into())
            }
            (CredentialView::ApiKey(value), None) if self.kind == Kind::Static => Ok(value.into()),
            (CredentialView::VertexServiceAccount { access, .. }, None)
                if self.kind == Kind::Vertex =>
            {
                Ok(access.into())
            }
            (
                CredentialView::OAuth {
                    access, account, ..
                },
                Some(expected),
            ) if self.kind == Kind::Static && account == expected.as_str() => Ok(access.into()),
            (CredentialView::OAuth { .. }, Some(_)) => Err(CredentialRefusal::AccountMismatch),
            _ => Err(CredentialRefusal::Vault(VaultError::Corrupt)),
        }
    }
    pub(super) fn is_vertex_adc(&self) -> bool {
        self.kind == Kind::VertexAdc
    }

    pub(super) fn check_adc(&self, root: &Path) -> Result<(), CredentialRefusal> {
        if !self.is_vertex_adc() {
            return Err(CredentialRefusal::GrantStorageMismatch);
        }
        let secret = SealedFileVault::open(root)
            .map_err(CredentialRefusal::Vault)?
            .get(&self.key, &self.binding)
            .map_err(CredentialRefusal::Vault)?
            .ok_or(CredentialRefusal::Absent)?;
        match secret.expose() {
            CredentialView::VertexAdc => Ok(()),
            _ => Err(CredentialRefusal::SourceChanged),
        }
    }

    pub(super) fn aws_selection(
        &self,
        root: &Path,
    ) -> Result<Option<grapt_login::aws::Selection>, CredentialRefusal> {
        if self.kind != Kind::Aws {
            return Ok(None);
        }
        let secret = SealedFileVault::open(root)
            .map_err(CredentialRefusal::Vault)?
            .get(&self.key, &self.binding)
            .map_err(CredentialRefusal::Vault)?
            .ok_or(CredentialRefusal::Absent)?;
        match secret.expose() {
            CredentialView::AwsChain(selection) => Ok(Some(selection.clone())),
            _ => Err(CredentialRefusal::Vault(VaultError::Corrupt)),
        }
    }

    pub(super) fn azure_cli(&self, root: &Path) -> Result<bool, CredentialRefusal> {
        if self.kind != Kind::AzureCli {
            return Ok(false);
        }
        let secret = SealedFileVault::open(root)
            .map_err(CredentialRefusal::Vault)?
            .get(&self.key, &self.binding)
            .map_err(CredentialRefusal::Vault)?
            .ok_or(CredentialRefusal::Absent)?;
        if matches!(secret.expose(), CredentialView::AzureCli) {
            Ok(true)
        } else {
            Err(CredentialRefusal::SourceChanged)
        }
    }

    pub(super) fn vertex_session(
        &self,
        root: &Path,
    ) -> Result<grapt_login::vertex::Session, CredentialRefusal> {
        if self.kind != Kind::Vertex {
            return Err(CredentialRefusal::GrantStorageMismatch);
        }
        let secret = SealedFileVault::open(root)
            .map_err(CredentialRefusal::Vault)?
            .get(&self.key, &self.binding)
            .map_err(CredentialRefusal::Vault)?
            .ok_or(CredentialRefusal::Absent)?;
        let CredentialView::VertexServiceAccount {
            key,
            access,
            expires_at,
        } = secret.expose()
        else {
            return Err(CredentialRefusal::Vault(VaultError::Corrupt));
        };
        let token = grapt_login::vertex::Token::from_cached(access.to_owned().into(), expires_at)
            .map_err(|_| CredentialRefusal::Vault(VaultError::Corrupt))?;
        grapt_login::vertex::Session::new(key.to_owned().into(), token)
            .map_err(|_| CredentialRefusal::Vault(VaultError::Corrupt))
    }
    pub(super) fn check_vertex_key(
        &self,
        root: &Path,
        session: &grapt_login::vertex::Session,
    ) -> Result<(), CredentialRefusal> {
        if self.kind != Kind::Vertex {
            return Err(CredentialRefusal::GrantStorageMismatch);
        }
        let secret = SealedFileVault::open(root)
            .map_err(CredentialRefusal::Vault)?
            .get(&self.key, &self.binding)
            .map_err(CredentialRefusal::Vault)?
            .ok_or(CredentialRefusal::Absent)?;
        match secret.expose() {
            CredentialView::VertexServiceAccount { key, .. } if session.matches_key(key) => Ok(()),
            _ => Err(CredentialRefusal::SourceChanged),
        }
    }
    pub(super) fn watsonx_session(
        &self,
        root: &Path,
    ) -> Result<grapt_login::watsonx::Session, CredentialRefusal> {
        if self.kind != Kind::Watsonx {
            return Err(CredentialRefusal::GrantStorageMismatch);
        }
        let secret = SealedFileVault::open(root)
            .map_err(CredentialRefusal::Vault)?
            .get(&self.key, &self.binding)
            .map_err(CredentialRefusal::Vault)?
            .ok_or(CredentialRefusal::Absent)?;
        let CredentialView::WatsonxIam {
            key,
            access,
            expires_at,
        } = secret.expose()
        else {
            return Err(CredentialRefusal::Vault(VaultError::Corrupt));
        };
        let token = grapt_login::watsonx::Token::from_cached(access.to_owned().into(), expires_at)
            .map_err(|_| CredentialRefusal::Vault(VaultError::Corrupt))?;
        grapt_login::watsonx::Session::new(key.to_owned(), token)
            .map_err(|_| CredentialRefusal::Vault(VaultError::Corrupt))
    }
    pub(super) fn check_watsonx_key(
        &self,
        root: &Path,
        session: &grapt_login::watsonx::Session,
    ) -> Result<(), CredentialRefusal> {
        if self.kind != Kind::Watsonx {
            return Err(CredentialRefusal::GrantStorageMismatch);
        }
        let secret = SealedFileVault::open(root)
            .map_err(CredentialRefusal::Vault)?
            .get(&self.key, &self.binding)
            .map_err(CredentialRefusal::Vault)?
            .ok_or(CredentialRefusal::Absent)?;
        match secret.expose() {
            CredentialView::WatsonxIam { key, .. } if session.matches_key(key) => Ok(()),
            _ => Err(CredentialRefusal::SourceChanged),
        }
    }
    pub(super) fn bind(
        &self,
        credential: OpenerCredential,
    ) -> Result<OpenerCredential, CredentialRefusal> {
        let credential = credential
            .bind_destination(self.binding.api_origin())
            .map_err(CredentialRefusal::Capability)?;
        let credential = match &self.messages {
            Some(destination) => credential
                .bind_messages_destination(
                    destination.as_str(),
                    grapt_provider::MessagesAuthMode::Bearer,
                )
                .map_err(CredentialRefusal::Capability),
            None => Ok(credential),
        }?;
        match &self.cloudflare {
            Some(scope) => credential
                .bind_cloudflare(scope.clone())
                .map_err(CredentialRefusal::Capability),
            None => Ok(credential),
        }
    }
}
