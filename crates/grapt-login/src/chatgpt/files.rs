//! The local inputs owned by one ChatGPT connection's serialized sign-in.
//! Copyright 2025 OpenAI. Modified by Grapt; see licenses/openai-codex/.
use super::*;

mod generation;
pub use generation::SignInFileLock;
mod vault;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInFiles {
    state: String,
    credential: String,
    lock: String,
    vault: Option<vault::VaultState>,
    generation: Option<grapt_types::CredentialVersion>,
}

impl SignInFiles {
    pub fn legacy() -> Self {
        Self {
            state: STATE_FILE.into(),
            credential: CREDENTIAL_FILE.into(),
            lock: LOCK_FILE.into(),
            vault: None,
            generation: None,
        }
    }

    /// References are direct private-profile children, never caller paths.
    /// Keep the historical pair on its established lock and switch recovery.
    pub fn new(state: &str, credential: &str) -> Result<Self, SignInError> {
        let local = |name: &str, suffix: &str| {
            !name.starts_with('.')
                && !name.contains("..")
                && name.len() <= 64
                && name.ends_with(suffix)
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        };
        if !local(state, ".oauth.json") || !local(credential, ".credential") {
            return Err(SignInError::Store(
                "invalid connection sign-in references".into(),
            ));
        }
        if state == STATE_FILE && credential == CREDENTIAL_FILE {
            return Ok(Self::legacy());
        }
        // A new namespace must never share either half of the legacy pair.
        if state == STATE_FILE || credential == CREDENTIAL_FILE {
            return Err(SignInError::Store(
                "connection sign-in overlaps legacy state".into(),
            ));
        }
        Ok(Self {
            state: state.into(),
            credential: credential.into(),
            lock: format!("{state}.lock"),
            vault: None,
            generation: None,
        })
    }

    /// The local state reference, without opening or granting access to it.
    pub fn state_file(&self) -> &str {
        &self.state
    }

    /// The empty, owner-only renewal lock reference, for retired-file cleanup.
    pub fn lock_file(&self) -> &str {
        &self.lock
    }

    pub(super) fn is_legacy(&self) -> bool {
        self.vault.is_none() && self.state == STATE_FILE && self.credential == CREDENTIAL_FILE
    }

    pub fn load(&self, profile: &Path) -> Result<Option<Tokens>, SignInError> {
        if let Some(vault) = &self.vault {
            self.check_generation(profile)?;
            let tokens = vault.load(profile)?;
            self.check_generation(profile)?;
            return Ok(tokens);
        }
        if !self.is_legacy() {
            crate::profile_security::verify_directory(profile).map_err(|error| {
                SignInError::Store(format!("connection profile refused: {:?}", error.kind()))
            })?;
        }
        read_bounded(&profile.join(&self.state), STATE_CAP)?
            .map(|bytes| decode(&bytes))
            .transpose()
    }

    pub(super) fn save(
        &self,
        profile: &Path,
        tokens: &Tokens,
        updated: Timestamp,
    ) -> Result<(), SignInError> {
        if let Some(vault) = &self.vault {
            self.check_generation(profile)?;
            return vault.save(profile, tokens, updated);
        }
        replace_beside(profile, &self.state, &encode(tokens), &self.credential)?;
        self.project(profile, tokens)
    }

    pub(super) fn project(&self, profile: &Path, tokens: &Tokens) -> Result<(), SignInError> {
        // Vaulted OAuth has one authoritative snapshot, without a raw access
        // token projection that could bypass its persisted binding.
        if self.vault.is_some() {
            return Ok(());
        }
        if self.is_legacy() {
            return project(profile, tokens);
        }
        match read_bounded(&profile.join(&self.credential), TOKEN_MAX) {
            Ok(Some(bytes)) if bytes == tokens.access.as_bytes() => Ok(()),
            _ => replace(profile, &self.credential, tokens.access.as_bytes()),
        }
    }

    pub(super) fn open_lock(&self, profile: &Path) -> Result<std::fs::File, SignInError> {
        let failed = |error: std::io::Error| {
            SignInError::Store(format!("connection lock refused: {:?}", error.kind()))
        };
        if !self.is_legacy() {
            crate::profile_security::verify_directory(profile).map_err(failed)?;
        }
        let path = profile.join(&self.lock);
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        #[cfg(windows)]
        std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 3);
        match options.open(&path) {
            Ok(file) => {
                crate::profile_security::protect_new_private_file(&path, &file).map_err(failed)?;
                Ok(file)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut options = std::fs::OpenOptions::new();
                options.read(true).write(true);
                #[cfg(unix)]
                std::os::unix::fs::OpenOptionsExt::custom_flags(
                    &mut options,
                    libc::O_NONBLOCK | libc::O_NOFOLLOW,
                );
                #[cfg(windows)]
                {
                    use std::os::windows::fs::OpenOptionsExt;
                    options.custom_flags(0x0020_0000).share_mode(3);
                }
                let file = options.open(&path).map_err(failed)?;
                crate::profile_security::verify_owner_only_file(&path, &file).map_err(failed)?;
                if file.metadata().map_err(failed)?.len() != 0 {
                    return Err(SignInError::Store("connection lock is not empty".into()));
                }
                Ok(file)
            }
            Err(error) => Err(failed(error)),
        }
    }
}

/// Publish a sign-in after its network flow, under only this connection's lock.
pub async fn complete_sign_in_with_files(
    profile: &Path,
    files: &SignInFiles,
    tokens: &Tokens,
    updated: Timestamp,
) -> Result<(), SignInError> {
    let _protection = crate::profile_security::ProtectionBatch::begin()
        .map_err(|error| write_failure("profile protection", "started", &error))?;
    let identity =
        same_file::Handle::from_path(profile).map_err(|_| SignInError::ProfileChanged)?;
    let _owner = own_files(profile, files).await?;
    if same_file::Handle::from_path(profile).map_err(|_| SignInError::ProfileChanged)? != identity {
        return Err(SignInError::ProfileChanged);
    }
    #[cfg(windows)]
    if files.is_legacy() {
        migration::recover(profile).map_err(migration_error)?;
    }
    files.save(profile, tokens, updated)
}
