//! Real profile persistence and a bounded loopback token issuer.
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine;
use grapt_platform::{CredentialResolutionRequest, PlatformOwner};
use grapt_types::Timestamp;
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

const NOW: i64 = 1_800_000_000_000;
const HOUR: i64 = 3_600_000;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Fixed;
impl ClockAuthority for Fixed {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(NOW)
    }
}

struct Profile(PathBuf);
impl Profile {
    fn new(label: &str, expiry: i64) -> TestResult<Self> {
        let root =
            std::env::temp_dir().join(format!("grapt-live-owner-{label}-{}", std::process::id()));
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("grapt.jsonc"), serde_json::json!({
            "store":{"authority":"initialize"},"product":{"build":"live-credential-tests"},
            "provider":{"id":"openai","model":"gpt-5.6","base_model":"gpt-5.6",
                "family":"gpt-5","protocol_family":"chatgpt-codex","route":"responses","account":"account-a",
                "credential_version":chatgpt::SIGN_IN_CREDENTIAL_VERSION,"policy_version":"live-credentials-v1","variants":[]}
        }).to_string())?;
        chatgpt::save(&root, &tokens("account-a", "first-wire-token", expiry))?;
        Ok(Self(root))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _result = std::fs::remove_dir_all(&self.0);
    }
}

fn tokens(account: &str, access: &str, expiry: i64) -> chatgpt::Tokens {
    chatgpt::Tokens {
        access: access.into(),
        refresh: "first-refresh-token".into(),
        expires_at: Timestamp::from_millis(expiry),
        account: account.into(),
    }
}

fn answer(expiry: i64) -> String {
    let encoded = |value: serde_json::Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
    };
    let token = format!(
        "{}.{}.sig",
        encoded(serde_json::json!({"alg":"none"})),
        encoded(serde_json::json!({"exp":expiry/1000,"chatgpt_account_id":"account-a"}))
    );
    serde_json::json!({"access_token":token,"refresh_token":"second-refresh-token"}).to_string()
}

struct Issuer {
    base: String,
    calls: Arc<AtomicUsize>,
    grants: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Issuer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Issuer {
    async fn start(replies: Vec<String>) -> TestResult<Self> {
        Self::with_hold(replies, None).await
    }

    async fn with_hold(
        replies: Vec<String>,
        hold: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    ) -> TestResult<Self> {
        assert!(replies.len() <= 8);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let grants = Arc::new(Mutex::new(Vec::new()));
        let recorded = grants.clone();
        let mut replies: VecDeque<_> = replies.into();
        let task = tokio::spawn(async move {
            for _ in 0..8 {
                let (mut stream, _) = listener
                    .accept()
                    .await
                    .unwrap_or_else(|error| panic!("issuer accept: {error}"));
                let mut bytes = Vec::new();
                let mut complete = false;
                for _ in 0..32 {
                    let mut buffer = [0; 1024];
                    let size = stream
                        .read(&mut buffer)
                        .await
                        .unwrap_or_else(|error| panic!("issuer read: {error}"));
                    assert!(size > 0, "issuer request ended before its body");
                    bytes.extend_from_slice(&buffer[..size]);
                    let request = String::from_utf8_lossy(&bytes);
                    if let Some((head, body)) = request.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|value| value.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            complete = true;
                            break;
                        }
                    }
                }
                assert!(
                    complete,
                    "issuer request exceeded its bound before completion"
                );
                assert!(bytes.len() <= 32 * 1024);
                assert!(String::from_utf8_lossy(&bytes).starts_with("POST /oauth/token "));
                let number = observed.fetch_add(1, Ordering::SeqCst);
                let request = String::from_utf8_lossy(&bytes);
                let (_, body) = request
                    .split_once("\r\n\r\n")
                    .unwrap_or_else(|| panic!("issuer request has no body"));
                recorded
                    .lock()
                    .unwrap_or_else(|_| panic!("issuer record poisoned"))
                    .push(body.to_owned());
                if let Some((consumed, release)) = &hold {
                    if number == 0 {
                        consumed.notify_one();
                        release.notified().await;
                    }
                }
                let reply = replies.pop_front().unwrap_or_else(|| "{}".into());
                let response = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len());
                stream
                    .write_all(response.as_bytes())
                    .await
                    .unwrap_or_else(|error| panic!("issuer write: {error}"));
            }
        });
        Ok(Self {
            base,
            calls,
            grants,
            task,
        })
    }
}

fn client(timeout: Option<Duration>) -> TestResult<SignInClient> {
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_platform_verifier()?
    .with_no_client_auth();
    let builder = SignInClient::builder()
        .no_proxy()
        .use_preconfigured_tls(tls);
    let builder = match timeout {
        Some(timeout) => builder.timeout(timeout),
        None => builder,
    };
    Ok(builder.build()?)
}

#[tokio::test]
async fn service_startup_renews_a_non_primary_connection_without_cli_setup() -> TestResult {
    use std::io::Write;
    let profile = Profile::new("service-startup-secondary", NOW + 72 * HOUR)?;
    grapt_login::profile_security::protect_directory(&profile.0)?;
    let files = chatgpt::SignInFiles::new("work.oauth.json", "work.credential")?;
    chatgpt::complete_sign_in_with_files(
        &profile.0,
        &files,
        &tokens("account-a", "work-before", NOW + HOUR),
        Timestamp::from_millis(NOW),
    )
    .await?;
    let path = profile.0.join("connections.json");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path)?;
    grapt_login::profile_security::protect_new_private_file(&path, &file)?;
    file.write_all(serde_json::json!({"schema":"secode.connections.v1", "connections":[
        {"id":"openai", "catalog_provider":"openai", "family":"chatgpt-codex", "method":{"kind":"oauth-device"},
         "label":"Personal", "endpoint":null, "account":"account-a", "route":"responses", "credential_version":"chatgpt-oauth-v1",
         "policy_version":"policy", "credential_file":"provider.credential", "discovery_file":"account-models.json", "oauth_file":"chatgpt.oauth.json"},
        {"id":"work", "catalog_provider":"openai", "family":"chatgpt-codex", "method":{"kind":"oauth-device"},
         "label":"Work", "endpoint":null, "account":"account-a", "route":"work-route", "credential_version":"work-oauth-v1",
         "policy_version":"policy", "credential_file":"work.credential", "discovery_file":"work-models.json", "oauth_file":"work.oauth.json"}
    ], "default_model":{"provider_id":"openai", "id":"gpt-5.6", "variant":null}}).to_string().as_bytes())?;
    drop(file);
    let listing = serde_json::json!({"models":[{"slug":"gpt-5.6", "context_window":128000, "supported_reasoning_levels":[]}]}).to_string();
    for name in ["account-models.json", "work-models.json"] {
        std::fs::write(profile.0.join(name), &listing)?;
    }
    let config = grapt_platform::PublishedConfig::load(&profile.0)?;
    let primary = chatgpt::load(&profile.0)?;
    let issuer = Issuer::start(vec![answer(NOW + 72 * HOUR)]).await?;
    crate::compose::renewal::renew(
        &profile.0,
        &config,
        None,
        &client(Some(Duration::from_secs(3)))?,
        &issuer.base,
        &Fixed,
    )
    .await?;
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(chatgpt::load(&profile.0)?, primary);
    assert_eq!(
        files.load(&profile.0)?.ok_or("work sign-in")?.refresh,
        "second-refresh-token"
    );
    Ok(())
}

#[tokio::test]
async fn a_changed_handover_build_refuses_before_startup_renewal() -> TestResult {
    let profile = Profile::new("startup-changed-handover-build", NOW + HOUR)?;
    let state_before = std::fs::read(profile.0.join(chatgpt::STATE_FILE))?;
    let credential_before = std::fs::read(profile.0.join(chatgpt::CREDENTIAL_FILE))?;
    let issuer = Issuer::start(vec![answer(NOW + 72 * HOUR)]).await?;
    let config = grapt_platform::PublishedConfig::load(&profile.0)?;
    let refusal = crate::compose::renewal::renew(
        &profile.0,
        &config,
        Some("retained-old-build"),
        &client(Some(Duration::from_secs(3)))?,
        &issuer.base,
        &Fixed,
    )
    .await
    .err()
    .ok_or("changed build admitted renewal")?;
    assert!(refusal
        .to_string()
        .contains(&grapt_platform::PlatformRefusal::HandoverBuildChanged.to_string()));
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        std::fs::read(profile.0.join(chatgpt::STATE_FILE))?,
        state_before
    );
    assert_eq!(
        std::fs::read(profile.0.join(chatgpt::CREDENTIAL_FILE))?,
        credential_before
    );
    Ok(())
}

fn owner(profile: &Profile, issuer: &Issuer) -> TestResult<LiveCredentials> {
    owner_with_client(profile, issuer, client(Some(Duration::from_secs(3)))?)
}

fn owner_with_client(
    profile: &Profile,
    issuer: &Issuer,
    client: SignInClient,
) -> TestResult<LiveCredentials> {
    let platform = match PlatformOwner::prepare(&profile.0) {
        Ok(value) => value,
        Err(_) if profile.0.join("database.sqlite3").is_file() => {
            let path = profile.0.join("grapt.jsonc");
            std::fs::write(
                &path,
                std::fs::read_to_string(&path)?.replace("initialize", "migrate"),
            )?;
            PlatformOwner::prepare(&profile.0)?
        }
        Err(error) => return Err(error.into()),
    };
    let parts = platform.into_provider_parts()?;
    let (_store, targets, _, authority, _) = parts.into_components();
    let target = targets.issue()?;
    let request = CredentialResolutionRequest::new(
        &target,
        authority.record().clone(),
        ProtocolFamily::ChatGptCodex,
        authority.placement(),
    );
    let (_initial, mut source) = authority.into_refreshable(request)?;
    let _baseline = source.current()?;
    Ok(LiveCredentials {
        state: Arc::new(Mutex::new(Some(State {
            source,
            cloud: None,
            sign_in: Some(SignIn {
                session: Session::open(&profile.0, target.account().as_str())?
                    .ok_or("missing sign-in")?,
                client,
                issuer: issuer.base.clone(),
            }),
            unaccepted_refresh: None,
            accepted_revision: None,
        }))),
        gate: Arc::new(tokio::sync::Semaphore::new(1)),
        clock: Arc::new(Fixed),
    })
}

#[tokio::test]
async fn concurrent_rejections_refresh_once_and_a_rejected_replacement_cannot_loop() -> TestResult {
    let profile = Profile::new("concurrent", NOW + HOUR)?;
    let issuer = Issuer::start(vec![answer(NOW + 24 * HOUR), answer(NOW + 48 * HOUR)]).await?;
    let owner = owner(&profile, &issuer)?;
    let before = owner.current().await?.revision;
    let (left, right) = tokio::join!(owner.recover(before), owner.recover(before));
    assert!(left? && right?);
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    let after = owner.current().await?.revision;
    assert_ne!(after, before);
    owner.accepted(before).await?; // A late old success cannot clear the new guard.
    assert!(!owner.recover(after).await?);
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    owner.accepted(after).await?;
    assert!(owner.recover(after).await?);
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[test]
fn a_configured_sign_in_cannot_fall_back_to_a_bare_credential_file() -> TestResult {
    let profile = Profile::new("missing-sign-in-generation", NOW + HOUR)?;
    let platform = PlatformOwner::prepare(&profile.0)?;
    let (_store, targets, _, authority, _) = platform.into_provider_parts()?.into_components();
    let target = targets.issue()?;
    let request = CredentialResolutionRequest::new(
        &target,
        authority.record().clone(),
        ProtocolFamily::ChatGptCodex,
        authority.placement(),
    );
    let (_initial, source) = authority.into_refreshable(request)?;
    // The published grant still requires sign-in even if the local marker
    // disappears. A surviving derived token does not replace its state owner.
    std::fs::remove_file(profile.0.join(chatgpt::STATE_FILE))?;
    std::fs::remove_file(profile.0.join("grapt.jsonc"))?;
    let result = LiveCredentials::open(source, ProtocolFamily::ChatGptCodex, Arc::new(Fixed));
    assert_eq!(
        result.err(),
        Some(chatgpt::SignInError::Unfinished.to_string()),
        "a configured sign-in fell back to a bare credential file"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn an_expiring_session_renews_before_the_provider_request() -> TestResult {
    let profile = Profile::new("expiry", NOW + 10_000)?;
    let consumed = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let issuer = Issuer::with_hold(
        vec![answer(NOW + 24 * HOUR)],
        Some((consumed.clone(), release.clone())),
    )
    .await?;
    // As in grapt-login's paused-clock fixtures, socket scheduling must not
    // auto-advance through an unrelated HTTP deadline. Production stays timed.
    let owner = owner_with_client(&profile, &issuer, client(None)?)?;
    let pending = owner.current();
    tokio::pin!(pending);
    tokio::select! {
        () = consumed.notified() => (),
        _ = &mut pending => panic!("renewal returned before the held issuer replied"),
    }
    tokio::time::advance(Duration::from_secs(30)).await;
    release.notify_one();
    let current = pending.await?;
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    assert!(!owner.recover(current.revision).await?);
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn a_running_owner_refuses_an_account_switch_before_loading_its_credential() -> TestResult {
    let profile = Profile::new("account-pin", NOW + HOUR)?;
    let issuer = Issuer::start(Vec::new()).await?;
    let owner = owner(&profile, &issuer)?;
    let before = owner.current().await?.revision;
    chatgpt::save(
        &profile.0,
        &tokens("account-b", "foreign-wire-token", NOW + HOUR),
    )?;
    assert!(matches!(
        owner.current().await,
        Err(CredentialRenewalRefusal::AccountChanged)
    ));
    assert!(matches!(
        owner.recover(before).await,
        Err(CredentialRenewalRefusal::AccountChanged)
    ));
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn an_external_generation_is_reloaded_without_a_second_refresh_grant() -> TestResult {
    let profile = Profile::new("external", NOW + HOUR)?;
    let issuer = Issuer::start(Vec::new()).await?;
    let owner = owner(&profile, &issuer)?;
    let before = owner.current().await?.revision;
    chatgpt::save(
        &profile.0,
        &tokens("account-a", "external-wire-token", NOW + 24 * HOUR),
    )?;
    assert!(owner.recover(before).await?);
    let replacement = owner.current().await?.revision;
    assert_ne!(replacement, before);
    assert!(
        matches!(owner.recover(replacement).await, Ok(false)),
        "a rejected replacement reloaded from another process minted another token"
    );
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn cancellation_returns_credential_state_before_the_next_request() -> TestResult {
    let profile = Profile::new("cancelled-custody", NOW + HOUR)?;
    let issuer = Issuer::start(Vec::new()).await?;
    let owner = owner(&profile, &issuer)?;
    let initial = owner.current().await?.revision;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(profile.0.join("chatgpt.lock"))?;
    assert!(lock.try_lock().is_ok(), "fixture profile lock was not free");
    let pending = tokio::time::timeout(Duration::from_millis(20), owner.current()).await;
    assert!(
        pending.is_err(),
        "credential preparation did not wait on the held profile lock"
    );
    drop(lock);
    let next = tokio::time::timeout(Duration::from_secs(3), owner.current()).await??;
    assert_eq!(
        next.revision, initial,
        "cancellation changed the credential generation"
    );
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn late_rejections_and_successes_do_not_rearm_an_accepted_replacement() -> TestResult {
    let profile = Profile::new("late-external-callbacks", NOW + HOUR)?;
    let issuer = Issuer::start(vec![answer(NOW + 48 * HOUR)]).await?;
    let owner = owner(&profile, &issuer)?;
    let before = owner.current().await?.revision;
    chatgpt::save(
        &profile.0,
        &tokens("account-a", "external-wire-token", NOW + 24 * HOUR),
    )?;
    assert!(owner.recover(before).await?);
    let replacement = owner.current().await?.revision;
    assert!(!owner.recover(replacement).await?);
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 0);
    owner.accepted(replacement).await?;
    owner.accepted(before).await?;
    assert!(owner.recover(before).await?);
    assert!(
        owner.recover(replacement).await?,
        "a late rejection re-armed an already accepted replacement"
    );
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn a_refresh_grant_with_unchanged_access_material_is_not_submitted_repeatedly() -> TestResult
{
    let profile = Profile::new("unchanged-access", NOW + HOUR)?;
    let mut reply: serde_json::Value = serde_json::from_str(&answer(NOW + 24 * HOUR))?;
    reply["id_token"] = reply["access_token"].clone();
    reply["access_token"] = serde_json::json!("first-wire-token");
    let issuer = Issuer::start(vec![reply.to_string()]).await?;
    let owner = owner(&profile, &issuer)?;
    let rejected = owner.current().await?.revision;
    assert!(!owner.recover(rejected).await?);
    assert!(
        matches!(owner.recover(rejected).await, Ok(false)),
        "an unchanged access token triggered a second refresh grant"
    );
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[expect(clippy::cognitive_complexity, reason = "legacy")]
async fn cancelled_grant_is_persisted(proactive: bool) -> TestResult {
    let label = if proactive {
        "cancelled-proactive-grant"
    } else {
        "timed-out-rejection-grant"
    };
    let expiry = if proactive { NOW + 10_000 } else { NOW + HOUR };
    let profile = Profile::new(label, expiry)?;
    let consumed = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let issuer = Issuer::with_hold(
        vec![answer(NOW + HOUR), answer(NOW + 2 * HOUR)],
        Some((consumed.clone(), release.clone())),
    )
    .await?;
    let owner = owner(&profile, &issuer)?;
    let rejected = if proactive {
        None
    } else {
        Some(owner.current().await?.revision)
    };
    let mut pending = Box::pin(async {
        match rejected {
            Some(revision) => owner.recover(revision).await.map(|_| ()),
            None => owner.current().await.map(|_| ()),
        }
    });
    tokio::select! {
        reached = tokio::time::timeout(Duration::from_secs(3), consumed.notified()) => reached?,
        result = &mut pending => panic!("renewal returned before the held issuer replied: {result:?}"),
    }
    if proactive {
        drop(pending);
    } else {
        assert!(tokio::time::timeout(Duration::from_millis(20), pending)
            .await
            .is_err());
    }
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(profile.0.join("chatgpt.lock"))?;
    assert!(
        matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "cancellation released the consumed refresh grant before persistence"
    );
    release.notify_one();
    let replacement = tokio::time::timeout(Duration::from_secs(3), owner.current()).await??;
    let stored = chatgpt::load(&profile.0)?.ok_or("missing rotated generation")?;
    assert_eq!(
        stored.refresh, "second-refresh-token",
        "cancelled refresh lost its rotated generation"
    );
    assert!(
        !owner.recover(replacement.revision).await?,
        "cancelled caller lost the replacement guard"
    );
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    owner.accepted(replacement.revision).await?;
    assert!(owner.recover(replacement.revision).await?);
    let grants = issuer.grants.lock().map_err(|_| "issuer record poisoned")?;
    assert_eq!(grants.len(), 2);
    assert!(grants[0].contains("refresh_token=first-refresh-token"));
    assert!(
        grants[1].contains("refresh_token=second-refresh-token"),
        "the consumed old refresh grant was reused"
    );
    Ok(())
}

#[tokio::test]
async fn cancellation_during_proactive_renewal_preserves_the_rotated_grant() -> TestResult {
    cancelled_grant_is_persisted(true).await
}

#[tokio::test]
async fn timeout_during_rejected_token_renewal_preserves_the_rotated_grant() -> TestResult {
    cancelled_grant_is_persisted(false).await
}

async fn short_lived_replacement_is_guarded(proactive: bool) -> TestResult {
    let label = if proactive {
        "short-proactive"
    } else {
        "short-rejected"
    };
    let profile = Profile::new(label, if proactive { NOW + 10_000 } else { NOW + HOUR })?;
    let issuer = Issuer::start(vec![answer(NOW + 30_000), answer(NOW + 2 * HOUR)]).await?;
    let owner = owner(&profile, &issuer)?;
    if !proactive {
        let initial = owner.current().await?.revision;
        assert!(owner.recover(initial).await?);
    }
    let replacement = owner.current().await?.revision;
    for _ in 0..3 {
        assert_eq!(
            owner.current().await?.revision,
            replacement,
            "a short-lived unaccepted replacement caused another proactive mint"
        );
        assert!(
            !owner.recover(replacement).await?,
            "a short-lived unaccepted replacement caused another rejection mint"
        );
    }
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    owner.accepted(replacement).await?;
    assert_ne!(owner.current().await?.revision, replacement);
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn a_short_lived_proactive_replacement_waits_for_acceptance_before_another_mint() -> TestResult
{
    short_lived_replacement_is_guarded(true).await
}

#[tokio::test]
async fn a_short_lived_rejected_token_replacement_waits_for_acceptance_before_another_mint(
) -> TestResult {
    short_lived_replacement_is_guarded(false).await
}

#[derive(Debug)]
struct Advancing(std::sync::atomic::AtomicI64);
impl ClockAuthority for Advancing {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

#[tokio::test]
async fn an_unaccepted_replacement_is_refused_after_its_actual_expiry() -> TestResult {
    let profile = Profile::new("short-expired", NOW + 10_000)?;
    let issuer = Issuer::start(vec![answer(NOW + 30_000)]).await?;
    let mut owner = owner(&profile, &issuer)?;
    let clock = Arc::new(Advancing(std::sync::atomic::AtomicI64::new(NOW)));
    owner.clock = clock.clone();
    let _replacement = owner.current().await?;
    clock.0.store(NOW + 30_000, Ordering::SeqCst);
    assert!(
        matches!(
            owner.current().await,
            Err(CredentialRenewalRefusal::Expired)
        ),
        "an expired unaccepted replacement was prepared for a provider request"
    );
    assert_eq!(issuer.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[path = "tests/startup_recovery.rs"]
mod startup_recovery;

#[path = "tests/generation.rs"]
mod generation;
