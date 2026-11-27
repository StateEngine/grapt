//! OAuth uses the same staged publication and recovery as API keys.
use super::*;
use grapt_login::vault::{ApiDestination, OAuthRefreshDestination};

impl Store {
    /// Publish an account-pinned OAuth snapshot after issuer and model validation.
    /// The material is sealed at its explicit state reference; no raw access-token
    /// projection is created. Preparation and metadata commit share the writer.
    #[allow(clippy::too_many_arguments)]
    pub fn install_oauth_with<T>(
        &mut self,
        records: &Connections,
        connection: &ProviderId,
        secret: VaultSecret,
        binding: &DestinationBinding,
        issuer: &str,
        listing: &[u8],
        updated: Timestamp,
        prepare: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<(T, InstallCommit)> {
        self.require_no_install()?;
        let account = match secret.expose() {
            CredentialView::OAuth { account, .. } => account,
            CredentialView::AzureCli
            | CredentialView::VertexAdc
            | CredentialView::AwsChain(_)
            | CredentialView::ApiKey(_)
            | CredentialView::VertexServiceAccount { .. }
            | CredentialView::WatsonxIam { .. } => {
                return Err(invalid("OAuth install requires OAuth material"))
            }
        };
        let expected = DestinationBinding::oauth(
            ApiDestination::parse(binding.api_origin()).map_err(vault_error)?,
            OAuthRefreshDestination::parse(issuer).map_err(vault_error)?,
        );
        if *binding != expected {
            return Err(invalid(
                "OAuth install does not match its API and issuer binding",
            ));
        }
        let _encoded = model::encode(records)?;
        if listing.is_empty() || listing.len() > LISTING_CAP {
            return Err(invalid("connection model listing exceeds its bound"));
        }
        let selected = records
            .connections
            .iter()
            .find(|record| &record.id == connection)
            .ok_or_else(|| invalid("installed connection is absent"))?;
        let intent = Intent {
            oauth: true,
            ..Intent::checked(connection, &selected.credential_file)?
        };
        if selected.method != AuthMethod::OauthDevice
            || selected.family != Family::ChatgptCodex
            || selected.account.as_str() != account
            || selected.credential_format
                != (CredentialFormat::VaultedOAuth {
                    binding: binding.clone(),
                })
            || selected.oauth_file.as_deref() != Some(intent.material().as_str())
            || selected.discovery_file != intent.listing()
        {
            return Err(invalid(
                "installed OAuth material does not match its connection",
            ));
        }
        self.install_material(
            records,
            connection,
            Some(secret),
            binding,
            listing,
            updated,
            intent,
            prepare,
        )
    }
}
