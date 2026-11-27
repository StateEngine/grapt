//! A reused OAuth slot cannot be adopted or rewritten by a retired grant.
use super::*;
use grapt_types::CredentialVersion;
use std::{fs::File, io, path::PathBuf};

/// The same exclusive file owner used by credential renewal.
#[derive(Debug)]
pub struct SignInFileLock {
    _file: File,
    profile: PathBuf,
    files: SignInFiles,
}

impl SignInFiles {
    pub fn with_generation(mut self, generation: CredentialVersion) -> Result<Self, SignInError> {
        if self.vault.is_none()
            || generation.as_str().len() > 256
            || generation.as_str().chars().any(char::is_control)
        {
            return Err(SignInError::Store("invalid OAuth owner generation".into()));
        }
        self.generation = Some(generation);
        Ok(self)
    }
    pub fn generation_file(&self) -> String {
        format!("{}.owner", self.state.trim_end_matches(".oauth.json"))
    }
    pub fn check_generation(&self, profile: &Path) -> Result<(), SignInError> {
        let Some(generation) = &self.generation else {
            return Ok(());
        };
        let stored = crate::profile_security::read_owner_only_file(
            &profile.join(self.generation_file()),
            256,
        )
        .map_err(|_| SignInError::Store("OAuth owner generation is unavailable".into()))?;
        if stored != generation.as_str().as_bytes() {
            return Err(SignInError::Store(
                "OAuth owner generation was retired".into(),
            ));
        }
        Ok(())
    }
    /// Installation and recovery take the same lock held by renewal, without
    /// waiting under the synchronous connection writer.
    pub fn try_lock(&self, profile: &Path) -> io::Result<SignInFileLock> {
        let profile = std::fs::canonicalize(profile)?;
        let file = self.open_lock(&profile).map_err(io::Error::other)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => {
                io::Error::new(io::ErrorKind::WouldBlock, "OAuth state is being renewed")
            }
            std::fs::TryLockError::Error(error) => error,
        })?;
        Ok(SignInFileLock {
            _file: file,
            profile,
            files: self.clone(),
        })
    }
}
impl SignInFileLock {
    /// Complete legacy recovery while retaining this exact renewal owner.
    pub fn recover_interrupted_switch(&self) -> io::Result<()> {
        if self.files.is_legacy() {
            super::super::switch::recover(&self.profile)?;
        }
        Ok(())
    }

    /// Publish a new fence only while retaining the renewal owner's lock.
    pub fn publish_generation(&self) -> io::Result<()> {
        let profile = &self.profile;
        let generation = self
            .files
            .generation
            .as_ref()
            .ok_or_else(|| io::Error::other("OAuth generation is absent"))?;
        match crate::profile_security::read_owner_only_file(
            &profile.join(self.files.generation_file()),
            256,
        ) {
            Ok(previous) if previous == generation.as_str().as_bytes() => {
                return Err(io::Error::other(
                    "OAuth installation must use a new owner generation",
                ))
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        crate::profile_security::replace_private_file(
            profile,
            &self.files.generation_file(),
            generation.as_str().as_bytes(),
        )
    }
}
