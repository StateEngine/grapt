//! Bounded connection publication, with recovery of uncommitted credential material.
//! No network operation belongs inside this synchronous lock owner.
use super::*;
use grapt_login::vault::{
    ApiDestination, CredentialVault, CredentialView, DestinationBinding, SealedFileVault, VaultKey,
    VaultSecret,
};
use grapt_types::{ProviderId, Timestamp};
use serde::{Deserialize, Serialize};

mod oauth;

const INTENT: &str = "connection-install.json";
const INTENT_CAP: usize = 1024;
const LISTING_CAP: usize = 4 * 1024 * 1024;

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Failure {
    IntentSync,
    MetadataSync,
    Cleanup,
}

pub(super) fn sync_directory(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(root)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    connection: String,
    credential: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    oauth: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    environment: bool,
}

impl Intent {
    fn checked(connection: &ProviderId, credential: &str) -> io::Result<Self> {
        let stem = credential
            .strip_suffix(".credential")
            .and_then(|name| name.strip_prefix("connect-"))
            .and_then(|name| name.strip_suffix("-a").or_else(|| name.strip_suffix("-b")))
            .ok_or_else(|| invalid("invalid connection install slot"))?;
        if stem.is_empty()
            || stem.len() > 40
            || !stem
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(invalid("invalid connection install slot"));
        }
        VaultKey::new(connection.clone(), credential).map_err(vault_error)?;
        Ok(Self {
            connection: connection.as_str().into(),
            credential: credential.into(),
            oauth: false,
            environment: false,
        })
    }

    fn material(&self) -> String {
        if self.oauth {
            format!(
                "{}.oauth.json",
                self.credential.trim_end_matches(".credential")
            )
        } else {
            self.credential.clone()
        }
    }

    fn listing(&self) -> String {
        format!("{}.json", self.credential.trim_end_matches(".credential"))
    }
}

/// Two slots keep storage bounded across repeated re-connects. Current
/// published material is never overwritten before the metadata commit.
pub fn next_key_files(
    connection: &ProviderId,
    current: Option<&Connection>,
    token: &str,
) -> io::Result<(String, String)> {
    let credential = current
        .and_then(|record| {
            Intent::checked(&record.id, &record.credential_file).ok()?;
            let name = record.credential_file.strip_suffix(".credential")?;
            let (stem, next) = if let Some(stem) = name.strip_suffix("-a") {
                (stem, "b")
            } else {
                (name.strip_suffix("-b")?, "a")
            };
            Some(format!("{stem}-{next}.credential"))
        })
        .unwrap_or_else(|| format!("connect-{token}-a.credential"));
    let intent = Intent::checked(connection, &credential)?;
    Ok((credential, intent.listing()))
}

#[derive(Debug)]
pub struct InstallCommit {
    /// Metadata is visible and live state must be retained. When present,
    /// crash durability is uncertain and the recovery intent is kept.
    pub durability_error: Option<io::ErrorKind>,
    /// The publication succeeded. An unfinished cleanup is retried under the
    /// next storage lock and must not turn this success into an ambiguous retry.
    pub cleanup_error: Option<io::ErrorKind>,
}

impl Store {
    /// Publish only an already checked API key and its validated listing.
    /// Failure before the metadata rename leaves the old connection usable;
    /// reopening discards the sole unfinished alternate slot. After rename,
    /// recovery retains the new pair. Other connections are never cleanup targets.
    pub fn install_key(
        &mut self,
        records: &Connections,
        connection: &ProviderId,
        secret: VaultSecret,
        binding: &DestinationBinding,
        listing: &[u8],
        updated: Timestamp,
    ) -> io::Result<InstallCommit> {
        self.install_key_with(
            records,
            connection,
            secret,
            binding,
            listing,
            updated,
            || Ok(()),
        )
        .map(|(_, commit)| commit)
    }

    /// Prepare the live generation from the staged files and proposed records
    /// before publishing metadata. The callback is synchronous by construction.
    #[allow(clippy::too_many_arguments)]
    pub fn install_key_with<T>(
        &mut self,
        records: &Connections,
        connection: &ProviderId,
        secret: VaultSecret,
        binding: &DestinationBinding,
        listing: &[u8],
        updated: Timestamp,
        prepare: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<(T, InstallCommit)> {
        self.require_no_install()?;
        if !matches!(
            secret.expose(),
            CredentialView::ApiKey(_)
                | CredentialView::AzureCli
                | CredentialView::AwsChain(_)
                | CredentialView::VertexAdc
                | CredentialView::VertexServiceAccount { .. }
                | CredentialView::WatsonxIam { .. }
        ) || *binding
            != DestinationBinding::key(
                ApiDestination::parse(binding.api_origin()).map_err(vault_error)?,
            )
        {
            return Err(invalid(
                "connection key install requires key material and an API-only binding",
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
        let google_adc = matches!(secret.expose(), CredentialView::VertexAdc);
        let aws_chain = matches!(secret.expose(), CredentialView::AwsChain(_));
        let azure_cli = matches!(secret.expose(), CredentialView::AzureCli);
        let material_matches = match (selected.family, secret.expose()) {
            (
                Family::VertexGemini | Family::VertexAnthropic,
                CredentialView::VertexServiceAccount { .. },
            )
            | (Family::WatsonxChat, CredentialView::WatsonxIam { .. }) => true,
            (Family::VertexGemini | Family::VertexAnthropic, CredentialView::VertexAdc) => true,
            (Family::VertexGemini | Family::VertexAnthropic | Family::WatsonxChat, _)
            | (
                _,
                CredentialView::VertexServiceAccount { .. } | CredentialView::WatsonxIam { .. },
            ) => false,
            (Family::BedrockConverse, CredentialView::AwsChain(_)) => true,
            (Family::OpenaiResponses, CredentialView::AzureCli) => true,
            (_, CredentialView::ApiKey(_)) => true,
            _ => false,
        };
        if !material_matches
            || selected.method
                != if azure_cli {
                    AuthMethod::AzureCli
                } else if google_adc {
                    AuthMethod::GoogleAdc
                } else if aws_chain {
                    AuthMethod::AwsChain
                } else {
                    AuthMethod::Key
                }
            || azure_cli
                != matches!(
                    selected.credential_format,
                    CredentialFormat::AzureCli { .. }
                )
            || google_adc
                != matches!(
                    selected.credential_format,
                    CredentialFormat::GoogleAdc { .. }
                )
            || aws_chain
                != matches!(
                    selected.credential_format,
                    CredentialFormat::AwsChain { .. }
                )
            || !matches!(&selected.credential_format,
                CredentialFormat::Vault { binding: selected_binding }
                | CredentialFormat::GoogleAdc { binding: selected_binding }
                | CredentialFormat::AwsChain { binding: selected_binding }
                | CredentialFormat::AzureCli { binding: selected_binding }
                | CredentialFormat::MessagesBearer { binding: selected_binding, .. }
                | CredentialFormat::CloudflareGateway { binding: selected_binding, .. }
                if selected_binding == binding)
            || selected.oauth_file.is_some()
        {
            return Err(invalid("installed key does not match its connection"));
        }
        let intent = Intent::checked(connection, &selected.credential_file)?;
        if selected.discovery_file != intent.listing() {
            return Err(invalid("installed key and discovery use different slots"));
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

    /// Store a verified environment reference and its listing, never its value.
    pub fn install_environment_with<T>(
        &mut self,
        records: &Connections,
        connection: &ProviderId,
        listing: &[u8],
        binding: &DestinationBinding,
        prepare: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<(T, InstallCommit)> {
        self.require_no_install()?;
        if listing.is_empty() || listing.len() > LISTING_CAP {
            return Err(invalid("connection model listing exceeds its bound"));
        }
        let selected = records
            .connections
            .iter()
            .find(|record| &record.id == connection)
            .ok_or_else(|| invalid("installed environment connection is absent"))?;
        if !matches!(selected.method, AuthMethod::Env { .. })
            || selected.credential_format != CredentialFormat::LegacyFile
            || selected.oauth_file.is_some()
        {
            return Err(invalid(
                "installed environment reference does not match its connection",
            ));
        }
        binding
            .check_api(
                selected
                    .endpoint
                    .as_deref()
                    .ok_or_else(|| invalid("environment connection endpoint is absent"))?,
            )
            .map_err(vault_error)?;
        let mut intent = Intent::checked(connection, &selected.credential_file)?;
        if selected.discovery_file != intent.listing() {
            return Err(invalid(
                "environment reference and discovery use different slots",
            ));
        }
        intent.environment = true;
        self.install_material(
            records,
            connection,
            None,
            binding,
            listing,
            Timestamp::from_millis(0),
            intent,
            prepare,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn install_material<T>(
        &mut self,
        records: &Connections,
        connection: &ProviderId,
        secret: Option<VaultSecret>,
        binding: &DestinationBinding,
        listing: &[u8],
        updated: Timestamp,
        intent: Intent,
        prepare: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<(T, InstallCommit)> {
        let old = self.load()?;
        if old.as_ref().is_some_and(|records| {
            records.connections.iter().any(|record| {
                record.credential_file == intent.credential
                    || record.discovery_file == intent.listing()
                    || record.oauth_file.as_deref() == Some(intent.material().as_str())
            })
        }) {
            return Err(invalid(
                "connection install would overwrite published material",
            ));
        }
        let oauth_files = if intent.oauth {
            let selected = records
                .connections
                .iter()
                .find(|record| &record.id == connection)
                .ok_or_else(|| invalid("installed OAuth connection is absent"))?;
            Some(
                grapt_login::chatgpt::SignInFiles::new(&intent.material(), &intent.credential)
                    .and_then(|files| files.with_vault(connection.clone(), binding.clone()))
                    .and_then(|files| files.with_generation(selected.credential_version.clone()))
                    .map_err(io::Error::other)?,
            )
        } else {
            None
        };
        let _oauth_lock = oauth_files
            .as_ref()
            .map(|files| files.try_lock(&self.root))
            .transpose()?;
        let encoded = serde_json::to_vec(&intent)
            .map_err(|_| invalid("invalid connection install intent"))?;
        // Recover only this fixed slot; a staged intent never authorizes deletion.
        discard(&self.root, &format!(".{INTENT}.new"), INTENT_CAP)?;
        profile_security::replace_private_file(&self.root, INTENT, &encoded)?;
        #[cfg(test)]
        self.inject_failure(Failure::IntentSync)?;
        sync_directory(&self.root)?;
        if let Some(files) = &oauth_files {
            discard(
                &self.root,
                &format!(".{}.new", files.generation_file()),
                256,
            )?;
            _oauth_lock
                .as_ref()
                .ok_or_else(|| invalid("OAuth state owner is absent"))?
                .publish_generation()?;
            sync_directory(&self.root)?;
        }
        if let Some(secret) = secret {
            let mut vault = SealedFileVault::open(&self.root).map_err(vault_error)?;
            let key = VaultKey::new(connection.clone(), &intent.material()).map_err(vault_error)?;
            vault
                .put(&key, binding, secret, updated)
                .map_err(vault_error)?;
        }
        discard(
            &self.root,
            &format!(".{}.new", intent.listing()),
            LISTING_CAP,
        )?;
        profile_security::replace_private_file(&self.root, &intent.listing(), listing)?;
        let prepared = prepare()?;
        let durability_error = self.publish(records)?;
        drop(_oauth_lock);
        // The commit already succeeded. Leaving a complete intent on a failed
        // cleanup is safe and recovery can retire it before the next mutation.
        Ok((
            prepared,
            InstallCommit {
                durability_error,
                cleanup_error: if durability_error.is_none() {
                    self.recover_install().err().map(|error| error.kind())
                } else {
                    None
                },
            },
        ))
    }

    pub(super) fn require_no_install(&self) -> io::Result<()> {
        self.require_no_removal()?;
        match profile_security::read_owner_only_file(&self.root.join(INTENT), INTENT_CAP) {
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "unfinished connection install must be recovered",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn recover_install(&mut self) -> io::Result<()> {
        #[cfg(test)]
        self.inject_failure(Failure::Cleanup)?;
        discard(&self.root, &format!(".{INTENT}.new"), INTENT_CAP)?;
        let bytes =
            match profile_security::read_owner_only_file(&self.root.join(INTENT), INTENT_CAP) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            };
        let intent: Intent = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("invalid connection install intent"))?;
        let connection = ProviderId::new(&intent.connection)
            .ok_or_else(|| invalid("invalid install connection identity"))?;
        if intent.environment && intent.oauth {
            return Err(invalid("install intent has conflicting credential kinds"));
        }
        let intent = Intent {
            oauth: intent.oauth,
            environment: intent.environment,
            ..Intent::checked(&connection, &intent.credential)?
        };
        let oauth_files = intent
            .oauth
            .then(|| grapt_login::chatgpt::SignInFiles::new(&intent.material(), &intent.credential))
            .transpose()
            .map_err(io::Error::other)?;
        let _oauth_lock = oauth_files
            .as_ref()
            .map(|files| files.try_lock(&self.root))
            .transpose()?;
        let records = self.load()?;
        let mut committed = false;
        if let Some(records) = records {
            for record in records.connections {
                if record.credential_file == intent.credential
                    || record.discovery_file == intent.listing()
                    || record.oauth_file.as_deref() == Some(intent.material().as_str())
                {
                    if record.id != connection
                        || record.credential_file != intent.credential
                        || record.discovery_file != intent.listing()
                        || intent.oauth != (record.method == AuthMethod::OauthDevice)
                        || intent.environment != matches!(record.method, AuthMethod::Env { .. })
                        || intent.oauth
                            && record.oauth_file.as_deref() != Some(intent.material().as_str())
                    {
                        return Err(invalid(
                            "install intent conflicts with a published connection",
                        ));
                    }
                    committed = true;
                }
            }
        }
        if !committed {
            if !intent.environment {
                let mut vault = SealedFileVault::open(&self.root).map_err(vault_error)?;
                let key = VaultKey::new(connection, &intent.material()).map_err(vault_error)?;
                vault.remove(&key).map_err(vault_error)?;
            }
            if let Some(files) = &oauth_files {
                discard(&self.root, &files.generation_file(), 256)?;
                discard(
                    &self.root,
                    &format!(".{}.new", files.generation_file()),
                    256,
                )?;
            }
            discard(&self.root, &intent.listing(), LISTING_CAP)?;
            discard(
                &self.root,
                &format!(".{}.new", intent.listing()),
                LISTING_CAP,
            )?;
        }
        discard(&self.root, INTENT, INTENT_CAP)?;
        sync_directory(&self.root)?;
        Ok(())
    }
}

fn discard(root: &Path, name: &str, bound: usize) -> io::Result<()> {
    match profile_security::read_owner_only_file(&root.join(name), bound) {
        Ok(_) => std::fs::remove_file(root.join(name)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn vault_error(error: grapt_login::vault::VaultError) -> io::Error {
    io::Error::other(error)
}
