//! Paused current/recover/startup workers cannot cross a committed disconnect.
use super::*;
use grapt_login::chatgpt::{SignInFileLock, SignInFiles, SignInLockAuthority};
use grapt_provider::CredentialRevision;
use std::sync::{
    atomic::AtomicBool,
    mpsc::{sync_channel, Receiver},
};

struct Paused {
    inner: Arc<dyn SignInLockAuthority>,
    entered: Arc<tokio::sync::Notify>,
    release: Mutex<Receiver<()>>,
    once: AtomicBool,
}
impl SignInLockAuthority for Paused {
    fn try_own(&self, profile: &Path, files: &SignInFiles) -> io::Result<SignInFileLock> {
        if !self.once.swap(true, Ordering::SeqCst) {
            self.entered.notify_one();
            self.release
                .lock()
                .map_err(|_| io::Error::other("fixture gate poisoned"))?
                // The controller performs real protected-store cleanup while
                // this synthetic gate holds the worker, including on Windows.
                .recv_timeout(Duration::from_secs(120))
                .map_err(|_| io::Error::other("fixture gate not released"))?;
        }
        self.inner.try_own(profile, files)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn paused_current_and_recovery_refuse_disconnect_without_recreating_locks() -> TestResult {
    for (completed_cleanup, operation) in [
        (false, "current"),
        (true, "current"),
        (false, "recover"),
        (true, "recover"),
        (false, "startup"),
        (true, "startup"),
    ] {
        let profile = Profile(std::env::temp_dir().join(format!(
            "grapt-disconnect-race-{}-{completed_cleanup}-{operation}",
            std::process::id()
        )));
        std::fs::create_dir(&profile.0)?;
        grapt_login::profile_security::protect_directory(&profile.0)?;
        std::fs::write(
            profile.0.join("grapt.jsonc"),
            br#"{"store":{"authority":"open-ready"},"product":{"build":"disconnect-race"}}"#,
        )?;
        std::fs::write(
            profile.0.join("model-catalog.json"),
            grapt_connections::catalog::BUNDLED,
        )?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let api = format!("http://{}", listener.local_addr()?);
        let issuer = Issuer::with_hold(vec![answer(NOW + 72 * HOUR)], None).await?;
        let binding = DestinationBinding::oauth(
            ApiDestination::parse(&api)?,
            OAuthRefreshDestination::parse(&issuer.base)?,
        );
        let records = proposed(
            Connections {
                connections: Vec::new(),
                default_model: None,
            },
            &binding,
            &api,
            1,
        )?;
        let _installed = install(
            &profile.0,
            &records,
            &issuer,
            &binding,
            "first-token",
            NOW + 10_000,
        )?;
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, waiting) = sync_channel(1);
        let observed = Arc::clone(&entered);
        let wrap = |inner: Arc<dyn SignInLockAuthority>| -> Arc<dyn SignInLockAuthority> {
            Arc::new(Paused {
                inner,
                entered: observed,
                release: Mutex::new(waiting),
                once: AtomicBool::new(false),
            })
        };
        let worker = if operation == "startup" {
            let config = grapt_platform::PublishedConfig::load(&profile.0)?;
            let (grant, files) = config.chatgpt_sign_ins().next().ok_or("startup grant")?;
            let mut session =
                crate::compose::renewal::open_session_with(&profile.0, grant, files, wrap)?;
            let client = client(Some(Duration::from_secs(30)))?;
            let endpoint = issuer.base.clone();
            tokio::spawn(async move {
                session
                    .prepare_startup(&client, &endpoint, &Fixed)
                    .await
                    .map(|_| ())
                    .map_err(|_| CredentialRenewalRefusal::Unavailable)
            })
        } else {
            let held = prepare_with(&profile.0, &records, &issuer, wrap)?;
            tokio::spawn(async move {
                if operation == "recover" {
                    held.owner
                        .recover(CredentialRevision::new(0))
                        .await
                        .map(|_| ())
                } else {
                    held.owner.current().await.map(|_| ())
                }
            })
        };
        tokio::time::timeout(Duration::from_secs(15), entered.notified()).await?;
        let removed = Connections {
            connections: Vec::new(),
            default_model: None,
        };
        if completed_cleanup {
            Store::open(&profile.0)?.remove_with(
                &records.connections[0].id,
                &removed,
                || Ok(()),
            )?;
        } else {
            // A visible commit with deferred cleanup retains the token files.
            Store::open(&profile.0)?.save(&removed)?;
            assert!(profile
                .0
                .join(
                    records.connections[0]
                        .oauth_file
                        .as_deref()
                        .ok_or("OAuth file")?
                )
                .exists());
        }
        release.send(()).map_err(|_| format!(
            "fixture owner ended before release ({operation}, completed_cleanup={completed_cleanup})"
        ))?;
        assert!(
            tokio::time::timeout(Duration::from_secs(15), worker)
                .await??
                .is_err(),
            "paused owner crossed a committed disconnect"
        );
        assert_eq!(issuer.calls.load(Ordering::SeqCst), 0);
        if !completed_cleanup {
            Store::open(&profile.0)?.save(&records)?;
            Store::open(&profile.0)?.remove_with(
                &records.connections[0].id,
                &removed,
                || Ok(()),
            )?;
        }
        for slot in ["a", "b"] {
            assert!(
                !profile
                    .0
                    .join(format!("connect-oauth-generation-{slot}.oauth.json.lock"))
                    .exists(),
                "paused worker recreated a retired lock"
            );
        }
        assert_eq!(Store::read_snapshot(&profile.0)?, Some(removed));
    }
    Ok(())
}
