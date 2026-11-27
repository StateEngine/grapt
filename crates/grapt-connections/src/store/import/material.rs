//! Typed credential capture; never copy a platform-protected stored envelope.
use super::*;
use grapt_login::{
    chatgpt::{self, SignInFiles},
    vault::{
        ApiDestination, CredentialVault, OAuthRefreshDestination, SealedFileVault, VaultKey,
        VaultSecret,
    },
};
use zeroize::Zeroizing;

pub(super) enum Material {
    Legacy(Zeroizing<Vec<u8>>),
    Environment,
    Vault {
        secret: VaultSecret,
        binding: DestinationBinding,
        oauth: bool,
        updated: Timestamp,
    },
}

impl Material {
    pub(super) fn capture(
        root: &Path,
        record: &Connection,
    ) -> io::Result<(Self, Option<(SignInFiles, SignInFileLock)>)> {
        if matches!(record.method, AuthMethod::Env { .. }) {
            return Ok((Self::Environment, None));
        }
        if record.method == AuthMethod::OauthDevice {
            let mut files = SignInFiles::new(
                record
                    .oauth_file
                    .as_deref()
                    .ok_or_else(|| invalid("OAuth reference is absent"))?,
                &record.credential_file,
            )
            .map_err(io::Error::other)?;
            let binding = match &record.credential_format {
                CredentialFormat::LegacyFile => DestinationBinding::oauth(
                    ApiDestination::parse(
                        record.endpoint.as_deref().unwrap_or(chatgpt::CODEX_BACKEND),
                    )
                    .map_err(io::Error::other)?,
                    OAuthRefreshDestination::parse(chatgpt::ISSUER).map_err(io::Error::other)?,
                ),
                CredentialFormat::Vault { binding }
                | CredentialFormat::VaultedOAuth { binding } => {
                    files = files
                        .with_vault(record.id.clone(), binding.clone())
                        .map_err(io::Error::other)?;
                    if matches!(
                        record.credential_format,
                        CredentialFormat::VaultedOAuth { .. }
                    ) {
                        files = files
                            .with_generation(record.credential_version.clone())
                            .map_err(io::Error::other)?;
                    }
                    binding.clone()
                }
                _ => return Err(invalid("OAuth credential format does not match its method")),
            };
            binding
                .check_api(record.endpoint.as_deref().unwrap_or(chatgpt::CODEX_BACKEND))
                .and_then(|()| binding.check_refresh(chatgpt::ISSUER))
                .map_err(io::Error::other)?;
            let lock = files.try_lock(root)?;
            if chatgpt::has_pending_switch(root)? {
                return Err(invalid("account sign-in switch is unfinished"));
            }
            let tokens = files
                .load(root)
                .map_err(io::Error::other)?
                .ok_or_else(|| invalid("account sign-in state is absent"))?;
            if record.account.as_str() != tokens.account {
                return Err(invalid("account sign-in and connection disagree"));
            }
            if record.credential_format == CredentialFormat::LegacyFile
                && plaintext(root, &record.credential_file)?.as_slice() != tokens.access.as_bytes()
            {
                return Err(invalid("account sign-in and access credential disagree"));
            }
            return Ok((
                Self::Vault {
                    secret: VaultSecret::oauth(tokens).map_err(io::Error::other)?,
                    updated: if record.credential_format == CredentialFormat::LegacyFile {
                        Timestamp::from_millis(0)
                    } else {
                        updated(root, record, files.state_file(), &binding)?
                    },
                    binding,
                    oauth: true,
                },
                Some((files, lock)),
            ));
        }
        let Some(binding) = record.credential_format.binding() else {
            return Ok((
                Self::Legacy(plaintext(root, &record.credential_file)?),
                None,
            ));
        };
        if let Some(endpoint) = &record.endpoint {
            binding.check_api(endpoint).map_err(io::Error::other)?;
        }
        let vault = SealedFileVault::open(root).map_err(io::Error::other)?;
        let key =
            VaultKey::new(record.id.clone(), &record.credential_file).map_err(io::Error::other)?;
        let secret = vault
            .get(&key, binding)
            .map_err(io::Error::other)?
            .ok_or_else(|| invalid("account credential is absent"))?;
        Ok((
            Self::Vault {
                secret,
                updated: updated(root, record, &record.credential_file, binding)?,
                binding: binding.clone(),
                oauth: false,
            },
            None,
        ))
    }
}

fn updated(
    root: &Path,
    record: &Connection,
    file: &str,
    binding: &DestinationBinding,
) -> io::Result<Timestamp> {
    SealedFileVault::open(root)
        .map_err(io::Error::other)?
        .describe(
            &VaultKey::new(record.id.clone(), file).map_err(io::Error::other)?,
            binding,
        )
        .map_err(io::Error::other)?
        .map(|description| description.updated)
        .ok_or_else(|| invalid("account credential changed during import"))
}

fn plaintext(root: &Path, name: &str) -> io::Result<Zeroizing<Vec<u8>>> {
    let bytes = Zeroizing::new(profile_security::read_owner_only_file(
        &root.join(name),
        profile_security::stored_bound(4096),
    )?);
    profile_security::open(&bytes, 4096).map(Zeroizing::new)
}

// Preserve the original stored generations only for comparison. Destination
// installation always consumes typed material and seals it for that profile.
pub(super) fn snapshots(
    root: &Path,
    record: &Connection,
) -> io::Result<Vec<(String, Zeroizing<Vec<u8>>)>> {
    let mut names = Vec::new();
    if !matches!(record.method, AuthMethod::Env { .. })
        && (record.method != AuthMethod::OauthDevice
            || record.credential_format == CredentialFormat::LegacyFile)
    {
        names.push(record.credential_file.clone());
    }
    if let Some(state) = &record.oauth_file {
        names.push(state.clone());
        if matches!(
            record.credential_format,
            CredentialFormat::VaultedOAuth { .. }
        ) {
            names.push(format!("{}.owner", state.trim_end_matches(".oauth.json")));
        }
    }
    names
        .into_iter()
        .map(|name| {
            let bytes = profile_security::read_owner_only_file(
                &root.join(&name),
                profile_security::stored_bound(32 * 1024),
            )?;
            Ok((name, Zeroizing::new(bytes)))
        })
        .collect()
}
