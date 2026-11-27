// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: one stable lock file per private service profile,
// redacted errors, separate file descriptions and injected bounded waits.

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LockError {
    #[error("MCP credential transaction lock is unavailable")]
    Io,
    #[error("MCP credential transaction lock deadline was reached")]
    TimedOut,
}
#[derive(Clone)]
pub struct CredentialLock {
    path: PathBuf,
}
impl std::fmt::Debug for CredentialLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialLock([REDACTED])")
    }
}
pub struct CredentialGuard {
    _file: File,
}
impl Drop for CredentialGuard {
    fn drop(&mut self) {
        // A concurrent process launch can briefly inherit this open file
        // description. Release custody when the owner finishes, before close.
        let _ = self._file.unlock();
    }
}
impl std::fmt::Debug for CredentialGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialGuard")
    }
}
impl CredentialLock {
    pub fn for_profile(root: &Path) -> Result<Self, LockError> {
        let root = root.canonicalize().map_err(|_| LockError::Io)?;
        if !root.is_dir() {
            return Err(LockError::Io);
        }
        Ok(Self {
            path: root.join("mcp-oauth.lock"),
        })
    }
    /// Hold from the authoritative credential reread through refresh and its
    /// committed replacement. Never unlink this file: that creates two locks.
    pub async fn acquire(&self, budget: Duration) -> Result<CredentialGuard, LockError> {
        if budget.is_zero() || budget > Duration::from_secs(60) {
            return Err(LockError::TimedOut);
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        // A fresh open, not try_clone: clones share a file description and can
        // accidentally treat a same-process contender as the existing owner.
        let file = options.open(&self.path).map_err(|_| LockError::Io)?;
        tokio::time::timeout(budget, async {
            loop {
                match file.try_lock() {
                    Ok(()) => return Ok(CredentialGuard { _file: file }),
                    Err(std::fs::TryLockError::WouldBlock) => {
                        tokio::time::sleep(Duration::from_millis(50)).await
                    }
                    Err(_) => return Err(LockError::Io),
                }
            }
        })
        .await
        .map_err(|_| LockError::TimedOut)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Result<Self, std::io::Error> {
            let root = std::env::temp_dir().join(format!(
                "grapt-oauth-lock-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&root)?;
            Ok(Self(root))
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[tokio::test]
    async fn independent_handles_serialize_and_cancelled_waiters_do_not_leak_a_lock(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let scratch = Scratch::new()?;
        let first = CredentialLock::for_profile(&scratch.0)?;
        let other = CredentialLock::for_profile(&scratch.0)?;
        let guard = first.acquire(Duration::from_secs(1)).await?;
        assert!(matches!(
            other.acquire(Duration::from_millis(20)).await,
            Err(LockError::TimedOut)
        ));
        let cancelled = other.clone();
        let waiter = tokio::spawn(async move { cancelled.acquire(Duration::from_secs(1)).await });
        tokio::task::yield_now().await;
        waiter.abort();
        let _ = waiter.await;
        drop(guard);
        let guard = other.acquire(Duration::from_secs(1)).await?;
        assert!(matches!(
            first.acquire(Duration::from_millis(20)).await,
            Err(LockError::TimedOut)
        ));
        drop(guard);
        assert!(scratch.0.join("mcp-oauth.lock").is_file());
        assert!(!format!("{first:?}").contains("grapt-oauth-lock"));
        Ok(())
    }
    #[tokio::test]
    async fn abrupt_process_exit_releases_the_stable_profile_lock(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let scratch = Scratch::new()?;
        let mut child = tokio::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "oauth::credential_lock::tests::profile_lock_crash_child",
                "--ignored",
                "--test-threads=1",
            ])
            .env("GRAPT_MCP_LOCK_FIXTURE_ROOT", &scratch.0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait()).await??;
        assert_eq!(
            status.code(),
            Some(87),
            "fixture child did not reach its named crash"
        );
        assert_eq!(
            std::fs::read_to_string(scratch.0.join("child-held"))?,
            "held"
        );
        let lock = CredentialLock::for_profile(&scratch.0)?;
        let _guard = lock.acquire(Duration::from_secs(1)).await?;
        assert!(scratch.0.join("mcp-oauth.lock").is_file());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "invoked only by the parent crash witness with a private fixture profile"]
    async fn profile_lock_crash_child() -> Result<(), Box<dyn std::error::Error>> {
        let root = PathBuf::from(std::env::var("GRAPT_MCP_LOCK_FIXTURE_ROOT")?);
        let lock = CredentialLock::for_profile(&root)?;
        let _guard = lock.acquire(Duration::from_secs(1)).await?;
        std::fs::write(root.join("child-held"), "held")?;
        std::process::exit(87)
    }
}
