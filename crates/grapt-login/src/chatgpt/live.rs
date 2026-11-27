//! Account-pinned renewal for a running provider owner.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::*;

// Renew before a bounded token request plus a provider response-head wait can
// consume the remaining lifetime. This is a scheduling margin, not token grace.
const LIVE_AHEAD_MILLIS: i64 = 60_000;

/// A managed profile admits renewal and takes its file owner in one
/// synchronous critical section. Busy owners are retried outside that section.
pub trait SignInLockAuthority: Send + Sync {
    fn try_own(&self, profile: &Path, files: &SignInFiles) -> std::io::Result<SignInFileLock>;
}
struct SessionLock {
    _ordinary: Option<std::fs::File>,
    _managed: Option<SignInFileLock>,
}

/// One running profile's original ChatGPT account. A changed account is never
/// adopted, even if another process signed in successfully into the same path.
#[allow(
    missing_debug_implementations,
    reason = "the sign-in owner retains credential material"
)]
pub struct Session {
    profile: PathBuf,
    account: String,
    known_access: zeroize::Zeroizing<String>,
    root_identity: same_file::Handle,
    files: SignInFiles,
    lock_authority: Option<Arc<dyn SignInLockAuthority>>,
}

impl Session {
    pub fn open(profile: &Path, account: &str) -> Result<Option<Self>, SignInError> {
        Self::open_with_files(profile, account, SignInFiles::legacy())
    }

    pub fn open_with_files(
        profile: &Path,
        account: &str,
        files: SignInFiles,
    ) -> Result<Option<Self>, SignInError> {
        let metadata =
            std::fs::symlink_metadata(profile).map_err(|_| SignInError::ProfileChanged)?;
        if !metadata.is_dir() || linked(&metadata) {
            return Err(SignInError::ProfileChanged);
        }
        let profile = std::fs::canonicalize(profile).map_err(|_| SignInError::ProfileChanged)?;
        let root_identity =
            same_file::Handle::from_path(&profile).map_err(|_| SignInError::ProfileChanged)?;
        if files.is_legacy() && !requires_renewal(&profile)? {
            return Ok(None);
        }
        let current = files.load(&profile)?.ok_or(SignInError::Unfinished)?;
        if current.account != account {
            return Err(SignInError::AccountChanged);
        }
        let session = Self {
            profile,
            account: current.account,
            known_access: zeroize::Zeroizing::new(current.access),
            root_identity,
            files,
            lock_authority: None,
        };
        session.validate_root()?;
        Ok(Some(session))
    }

    pub fn with_lock_authority(mut self, authority: Arc<dyn SignInLockAuthority>) -> Self {
        self.lock_authority = Some(authority);
        self
    }
    async fn own(&self) -> Result<SessionLock, SignInError> {
        let Some(authority) = &self.lock_authority else {
            return own_files(&self.profile, &self.files)
                .await
                .map(|file| SessionLock {
                    _ordinary: Some(file),
                    _managed: None,
                });
        };
        let deadline = tokio::time::Instant::now() + LOCK_ALLOWANCE;
        loop {
            match authority.try_own(&self.profile, &self.files) {
                Ok(owner) => {
                    return Ok(SessionLock {
                        _ordinary: None,
                        _managed: Some(owner),
                    })
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(LOCK_POLL).await
                }
                Err(error) => {
                    return Err(SignInError::Store(format!(
                        "sign-in admission refused: {:?}",
                        error.kind()
                    )))
                }
            }
        }
    }

    fn validate_root(&self) -> Result<(), SignInError> {
        let metadata =
            std::fs::symlink_metadata(&self.profile).map_err(|_| SignInError::ProfileChanged)?;
        if !metadata.is_dir() || linked(&metadata) {
            return Err(SignInError::ProfileChanged);
        }
        let current =
            same_file::Handle::from_path(&self.profile).map_err(|_| SignInError::ProfileChanged)?;
        if current != self.root_identity {
            return Err(SignInError::ProfileChanged);
        }
        Ok(())
    }

    fn current(&self) -> Result<Tokens, SignInError> {
        self.validate_root()?;
        let current = self
            .files
            .load(&self.profile)?
            .ok_or(SignInError::Unfinished)?;
        self.validate_root()?;
        if current.account != self.account {
            return Err(SignInError::AccountChanged);
        }
        Ok(current)
    }

    pub async fn prepare(
        &mut self,
        client: &reqwest::Client,
        issuer: &str,
        clock: &(dyn ClockAuthority + Sync),
    ) -> Result<Renewal, SignInError> {
        self.prepare_with(client, issuer, clock, true, |renewal| renewal)
            .await
    }

    /// Capture the provider snapshot while the same profile lock still guards
    /// account validation and renewal. The capture is synchronous and cannot
    /// leave a gap in which another sign-in replaces the derived token.
    /// An unaccepted replacement may be captured while valid, but cannot mint
    /// again merely because it falls inside the proactive renewal margin.
    pub async fn prepare_with<T>(
        &mut self,
        client: &reqwest::Client,
        issuer: &str,
        clock: &(dyn ClockAuthority + Sync),
        allow_refresh: bool,
        capture: impl FnOnce(Renewal) -> T + Send,
    ) -> Result<T, SignInError> {
        self.validate_root()?;
        let _owner = self.own().await?;
        let renewal = self
            .prepare_owned(client, issuer, clock, allow_refresh, LIVE_AHEAD_MILLIS)
            .await?;
        Ok(capture(renewal))
    }

    pub async fn recover(
        &mut self,
        client: &reqwest::Client,
        issuer: &str,
        clock: &(dyn ClockAuthority + Sync),
        allow_refresh: bool,
    ) -> Result<(), SignInError> {
        self.recover_with(client, issuer, clock, allow_refresh, || ())
            .await
    }

    /// Reload, optionally renew, and capture under one cross-process lock.
    pub async fn recover_with<T>(
        &mut self,
        client: &reqwest::Client,
        issuer: &str,
        clock: &(dyn ClockAuthority + Sync),
        allow_refresh: bool,
        capture: impl FnOnce() -> T + Send,
    ) -> Result<T, SignInError> {
        self.validate_root()?;
        let _owner = self.own().await?;
        self.recover_owned(client, issuer, clock, allow_refresh)
            .await?;
        Ok(capture())
    }

    /// Re-read under the existing cross-process lock before deciding to mint.
    /// An interrupted state-to-credential projection is repaired here too.
    async fn prepare_owned(
        &mut self,
        client: &reqwest::Client,
        issuer: &str,
        clock: &(dyn ClockAuthority + Sync),
        allow_refresh: bool,
        ahead_millis: i64,
    ) -> Result<Renewal, SignInError> {
        let current = self.current()?;
        let now = clock.now().as_millis();
        if !allow_refresh && current.expires_at.as_millis() <= now {
            return Err(SignInError::Expired(Box::new(SignInError::Malformed(
                "the stored ChatGPT sign-in",
            ))));
        }
        if !allow_refresh || current.expires_at.as_millis().saturating_sub(now) > ahead_millis {
            self.files.project(&self.profile, &current)?;
            self.known_access = zeroize::Zeroizing::new(current.access);
            return Ok(Renewal::Current);
        }
        self.files.check_refresh(issuer)?;
        match refresh(client, issuer, &current, clock).await {
            Ok(renewed) => {
                self.validate_root()?;
                self.files.save(&self.profile, &renewed, clock.now())?;
                self.known_access = zeroize::Zeroizing::new(renewed.access);
                Ok(Renewal::Refreshed)
            }
            Err(error) if current.expires_at.as_millis() > clock.now().as_millis() => {
                self.validate_root()?;
                self.files.project(&self.profile, &current)?;
                self.known_access = zeroize::Zeroizing::new(current.access);
                Ok(Renewal::Deferred(error))
            }
            Err(error) => Err(SignInError::Expired(Box::new(error))),
        }
    }

    /// Reload a rotated generation first; only the credential owner may then
    /// mint a replacement. The caller separately proves that the installed
    /// provider credential changed before allowing another physical attempt.
    async fn recover_owned(
        &mut self,
        client: &reqwest::Client,
        issuer: &str,
        clock: &(dyn ClockAuthority + Sync),
        allow_refresh: bool,
    ) -> Result<(), SignInError> {
        let current = self.current()?;
        if current.access.as_str() != self.known_access.as_str() {
            if current.expires_at.as_millis() <= clock.now().as_millis() {
                return Err(SignInError::Expired(Box::new(SignInError::Malformed(
                    "the stored ChatGPT sign-in",
                ))));
            }
            self.files.project(&self.profile, &current)?;
            self.known_access = zeroize::Zeroizing::new(current.access);
            return Ok(());
        }
        if !allow_refresh {
            self.files.project(&self.profile, &current)?;
            return Ok(());
        }
        self.files.check_refresh(issuer)?;
        let renewed = refresh(client, issuer, &current, clock).await?;
        self.validate_root()?;
        self.files.save(&self.profile, &renewed, clock.now())?;
        self.known_access = zeroize::Zeroizing::new(renewed.access);
        Ok(())
    }

    /// Direct service launches apply the same proactive margin as CLI starts.
    pub async fn prepare_startup(
        &mut self,
        client: &reqwest::Client,
        issuer: &str,
        clock: &(dyn ClockAuthority + Sync),
    ) -> Result<Renewal, SignInError> {
        self.validate_root()?;
        let _owner = self.own().await?;
        #[cfg(windows)]
        if self.files.is_legacy() {
            migration::recover(&self.profile).map_err(migration_error)?;
            let current = self.current()?;
            migration::migrate(&self.profile, &current).map_err(migration_error)?;
        }
        self.prepare_owned(
            client,
            issuer,
            clock,
            true,
            i64::try_from(REFRESH_AHEAD.as_millis()).unwrap_or(i64::MAX),
        )
        .await
    }
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}
