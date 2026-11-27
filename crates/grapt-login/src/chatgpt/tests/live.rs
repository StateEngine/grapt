use super::*;

#[tokio::test]
async fn connection_file_pairs_renew_and_pin_accounts_independently(
) -> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::new("connection-file-pairs");
    crate::profile_security::protect_directory(&profile.0)?;
    let first = SignInFiles::new("subscription-a.oauth.json", "subscription-a.credential")?;
    let second = SignInFiles::new("subscription-b.oauth.json", "subscription-b.credential")?;
    assert!(SignInFiles::new("../outside.oauth.json", "outside.credential").is_err());
    assert!(SignInFiles::new(STATE_FILE, "subscription-a.credential").is_err());
    let a = signed_in(NOW + HOUR);
    let mut b = signed_in(NOW + 72 * HOUR);
    b.account = "account-b".into();
    b.access = access("account-b", NOW + 72 * HOUR);
    complete_sign_in_with_files(&profile.0, &first, &a, Timestamp::from_millis(NOW)).await?;
    complete_sign_in_with_files(&profile.0, &second, &b, Timestamp::from_millis(NOW)).await?;
    let mut a_session = Session::open_with_files(&profile.0, "account-a", first.clone())?
        .ok_or("missing first sign-in")?;
    let mut b_session = Session::open_with_files(&profile.0, "account-b", second.clone())?
        .ok_or("missing second sign-in")?;
    let issuer = Issuer::start(vec![(
        "/oauth/token",
        200,
        token_answer("account-a", NOW + 72 * HOUR, Some("a-rotated")),
    )])
    .await;
    assert!(matches!(
        a_session
            .prepare_startup(&client(), &issuer.base, &Fixed(NOW))
            .await?,
        Renewal::Refreshed
    ));
    assert_eq!(second.load(&profile.0)?, Some(b.clone()));
    let first_lock = first.open_lock(&profile.0)?;
    first_lock.try_lock()?;
    tokio::time::timeout(
        Duration::from_secs(1),
        b_session.prepare(&client(), &issuer.base, &Fixed(NOW)),
    )
    .await??;
    drop(first_lock);
    assert_eq!(issuer.seen().len(), 1);
    assert_eq!(
        first.load(&profile.0)?.ok_or("first state")?.refresh,
        "a-rotated"
    );
    assert!(!profile.0.join(STATE_FILE).exists() && !profile.0.join(CREDENTIAL_FILE).exists());
    b.account = "foreign-account".into();
    complete_sign_in_with_files(&profile.0, &second, &b, Timestamp::from_millis(NOW)).await?;
    assert!(matches!(
        b_session
            .prepare(&client(), &issuer.base, &Fixed(NOW))
            .await,
        Err(SignInError::AccountChanged)
    ));
    Ok(())
}

#[derive(Debug)]
struct MovingClock(std::sync::atomic::AtomicI64);
impl ClockAuthority for MovingClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(std::sync::atomic::Ordering::SeqCst))
    }
}

#[tokio::test]
async fn a_token_that_expires_while_renewal_waits_is_not_used_as_a_deferred_success() {
    let profile = Profile::new("live-expiry-during-wait");
    save(&profile.0, &signed_in(NOW + 30_000)).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    let (issuer, entered, release) =
        Issuer::start_held(vec![("/oauth/token", 503, "{}".into())]).await;
    let clock = MovingClock(std::sync::atomic::AtomicI64::new(NOW));
    let http = client();
    let pending = session.prepare(&http, &issuer.base, &clock);
    tokio::pin!(pending);
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut pending => panic!("renewal finished before expiry: {result:?}"),
            _ = entered.notified() => {}
        }
    })
    .await
    .unwrap_or_else(|_| panic!("issuer was not reached before expiry"));
    clock
        .0
        .store(NOW + 60_000, std::sync::atomic::Ordering::SeqCst);
    release.notify_one();
    assert!(
        matches!(pending.await, Err(SignInError::Expired(_))),
        "a token expired during the failed renewal was called current"
    );
}

#[cfg(unix)]
struct ReplacedRoot {
    current: PathBuf,
    original: PathBuf,
}
#[cfg(unix)]
impl Drop for ReplacedRoot {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.current);
        let _restored = std::fs::rename(&self.original, &self.current);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_running_session_refuses_a_replaced_profile_directory() {
    let profile = Profile::new("live-root-pin");
    save(&profile.0, &signed_in(NOW + HOUR)).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    let displaced = profile.0.with_extension("displaced");
    std::fs::rename(&profile.0, &displaced).unwrap_or_else(|error| panic!("move fixture: {error}"));
    let _root = ReplacedRoot {
        current: profile.0.clone(),
        original: displaced,
    };
    std::fs::create_dir(&profile.0).unwrap_or_else(|error| panic!("replacement fixture: {error}"));
    save(&profile.0, &signed_in(NOW + 12 * HOUR)).unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(Vec::new()).await;
    assert!(matches!(
        session.prepare(&client(), &issuer.base, &Fixed(NOW)).await,
        Err(SignInError::ProfileChanged)
    ));
    assert!(matches!(
        session
            .recover(&client(), &issuer.base, &Fixed(NOW), true)
            .await,
        Err(SignInError::ProfileChanged)
    ));
    assert!(issuer.seen().is_empty());
}

fn running(profile: &Profile) -> Session {
    Session::open(&profile.0, "account-a")
        .unwrap_or_else(|error| panic!("open sign-in: {error}"))
        .unwrap_or_else(|| panic!("signed-in profile missing"))
}

#[tokio::test]
async fn an_already_expired_replacement_is_not_published_as_a_renewal() {
    let profile = Profile::new("live-expired-replacement");
    let previous = signed_in(NOW - HOUR);
    save(&profile.0, &previous).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    let issuer = Issuer::start(vec![(
        "/oauth/token",
        200,
        token_answer("account-a", NOW - 1000, Some("refresh-2")),
    )])
    .await;
    assert!(matches!(
        session.prepare(&client(), &issuer.base, &Fixed(NOW)).await,
        Err(SignInError::Expired(_))
    ));
    assert_eq!(
        load(&profile.0).unwrap_or_else(|error| panic!("{error}")),
        Some(previous)
    );
}

#[tokio::test]
async fn provider_snapshot_capture_stays_inside_the_profile_lock() {
    let profile = Profile::new("live-capture-lock");
    let current = signed_in(NOW + HOUR);
    save(&profile.0, &current).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    let issuer = Issuer::start(Vec::new()).await;
    let capture = || {
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(profile.0.join(LOCK_FILE))
            .unwrap_or_else(|error| panic!("lock contender: {error}"));
        assert!(
            matches!(contender.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
            "snapshot capture ran after releasing the profile lock"
        );
        assert_eq!(profile.read(CREDENTIAL_FILE), current.access);
    };
    session
        .prepare_with(&client(), &issuer.base, &Fixed(NOW), true, |_| capture())
        .await
        .unwrap_or_else(|error| panic!("prepare and capture: {error}"));
    session
        .recover_with(&client(), &issuer.base, &Fixed(NOW), false, capture)
        .await
        .unwrap_or_else(|error| panic!("recover and capture: {error}"));
    let after = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(profile.0.join(LOCK_FILE))
        .unwrap_or_else(|error| panic!("lock after capture: {error}"));
    assert!(after.try_lock().is_ok(), "capture leaked its profile lock");
    assert!(issuer.seen().is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn a_root_replaced_during_renewal_is_not_written_after_the_authority_replies() {
    let profile = Profile::new("live-root-during-refresh");
    save(&profile.0, &signed_in(NOW - HOUR)).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    let (issuer, entered, release) = Issuer::start_held(vec![(
        "/oauth/token",
        200,
        token_answer("account-a", NOW + 12 * HOUR, Some("refresh-2")),
    )])
    .await;
    let http = client();
    let clock = Fixed(NOW);
    let pending = session.prepare(&http, &issuer.base, &clock);
    tokio::pin!(pending);
    tokio::time::timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut pending => panic!("renewal finished before the root race: {result:?}"),
            _ = entered.notified() => {}
        }
    })
    .await
    .unwrap_or_else(|_| panic!("issuer was not reached before the root race"));
    assert_eq!(issuer.seen().len(), 1, "issuer was never contacted");
    let displaced = profile.0.with_extension("displaced");
    std::fs::rename(&profile.0, &displaced).unwrap_or_else(|error| panic!("move fixture: {error}"));
    let _root = ReplacedRoot {
        current: profile.0.clone(),
        original: displaced,
    };
    std::fs::create_dir(&profile.0).unwrap_or_else(|error| panic!("replacement fixture: {error}"));
    save(&profile.0, &signed_in(NOW + 24 * HOUR)).unwrap_or_else(|error| panic!("{error}"));
    release.notify_one();
    assert!(matches!(pending.await, Err(SignInError::ProfileChanged)));
    assert_eq!(
        profile.read(CREDENTIAL_FILE),
        access("account-a", NOW + 24 * HOUR),
        "renewal wrote into the replacement directory"
    );
}

#[tokio::test]
async fn a_running_session_renews_after_its_injected_clock_passes_expiry() {
    let profile = Profile::new("live-expiry");
    save(&profile.0, &signed_in(NOW + HOUR)).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    let issuer = Issuer::start(vec![(
        "/oauth/token",
        200,
        token_answer("account-a", NOW + 12 * HOUR, Some("refresh-2")),
    )])
    .await;
    assert!(matches!(
        session.prepare(&client(), &issuer.base, &Fixed(NOW)).await,
        Ok(Renewal::Current)
    ));
    assert!(issuer.seen().is_empty());
    assert!(matches!(
        session
            .prepare(&client(), &issuer.base, &Fixed(NOW + 2 * HOUR))
            .await,
        Ok(Renewal::Refreshed)
    ));
    assert_eq!(issuer.seen().len(), 1);
    assert_eq!(
        profile.read(CREDENTIAL_FILE),
        access("account-a", NOW + 12 * HOUR)
    );
}

#[tokio::test]
async fn rejection_adopts_an_external_generation_before_minting() {
    let profile = Profile::new("live-external-generation");
    save(&profile.0, &signed_in(NOW + HOUR)).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    let rotated = signed_in(NOW + 12 * HOUR);
    save(&profile.0, &rotated).unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(Vec::new()).await;
    session
        .recover(&client(), &issuer.base, &Fixed(NOW), true)
        .await
        .unwrap_or_else(|error| panic!("reconcile: {error}"));
    assert!(
        issuer.seen().is_empty(),
        "a rotated generation was minted again"
    );
    assert_eq!(profile.read(CREDENTIAL_FILE), rotated.access);
}

#[tokio::test]
async fn another_account_cannot_be_adopted_by_a_running_session() {
    let profile = Profile::new("live-account-pin");
    save(&profile.0, &signed_in(NOW + HOUR)).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    let mut foreign = signed_in(NOW + 12 * HOUR);
    foreign.account = "account-b".into();
    foreign.access = access("account-b", NOW + 12 * HOUR);
    save(&profile.0, &foreign).unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(Vec::new()).await;
    assert!(matches!(
        session.prepare(&client(), &issuer.base, &Fixed(NOW)).await,
        Err(SignInError::AccountChanged)
    ));
    assert!(matches!(
        session
            .recover(&client(), &issuer.base, &Fixed(NOW), true)
            .await,
        Err(SignInError::AccountChanged)
    ));
    assert!(issuer.seen().is_empty());
    assert_eq!(
        profile.read(CREDENTIAL_FILE),
        foreign.access,
        "refusing adoption must not undo the user's new sign-in"
    );
}

#[tokio::test]
async fn no_mint_reconciliation_repairs_the_derived_file_from_authoritative_state() {
    let profile = Profile::new("live-projection-repair");
    let current = signed_in(NOW + HOUR);
    save(&profile.0, &current).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    std::fs::write(
        profile.0.join(CREDENTIAL_FILE),
        b"unrelated-derived-material",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(Vec::new()).await;
    session
        .recover(&client(), &issuer.base, &Fixed(NOW), false)
        .await
        .unwrap_or_else(|error| panic!("reconcile: {error}"));
    assert!(issuer.seen().is_empty());
    assert_eq!(profile.read(CREDENTIAL_FILE), current.access);
}

#[tokio::test]
async fn concurrent_rejections_submit_the_old_refresh_token_only_once() {
    let profile = Profile::new("live-concurrent-rejections");
    save(&profile.0, &signed_in(NOW + HOUR)).unwrap_or_else(|error| panic!("{error}"));
    let mut first = running(&profile);
    let mut second = running(&profile);
    let issuer = Issuer::start_delayed(
        vec![(
            "/oauth/token",
            200,
            token_answer("account-a", NOW + 12 * HOUR, Some("refresh-2")),
        )],
        Duration::from_millis(20),
    )
    .await;
    let http = client();
    let clock = Fixed(NOW);
    let (left, right) = tokio::join!(
        first.recover(&http, &issuer.base, &clock, true),
        second.recover(&http, &issuer.base, &clock, true),
    );
    left.unwrap_or_else(|error| panic!("first recovery: {error}"));
    right.unwrap_or_else(|error| panic!("second recovery: {error}"));
    let seen = issuer.seen();
    assert_eq!(seen.len(), 1, "the refresh grant was replayed");
    assert!(seen[0].body.contains("refresh_token=refresh-1"));
    assert_eq!(
        profile.read(CREDENTIAL_FILE),
        access("account-a", NOW + 12 * HOUR)
    );
}

#[tokio::test]
async fn a_running_owner_repairs_a_crash_between_state_and_projection() {
    let profile = Profile::new("live-state-crash");
    save(&profile.0, &signed_in(NOW + HOUR)).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    let next = signed_in(NOW + 12 * HOUR);
    replace(&profile.0, STATE_FILE, &encode(&next)).unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(Vec::new()).await;
    session
        .prepare(&client(), &issuer.base, &Fixed(NOW))
        .await
        .unwrap_or_else(|error| panic!("crash reconciliation: {error}"));
    assert!(issuer.seen().is_empty());
    assert_eq!(profile.read(CREDENTIAL_FILE), next.access);
}

#[tokio::test]
async fn a_failed_live_renewal_of_expired_tokens_keeps_the_generation_and_stops() {
    let profile = Profile::new("live-expired-refusal");
    let current = signed_in(NOW - HOUR);
    save(&profile.0, &current).unwrap_or_else(|error| panic!("{error}"));
    let mut session = running(&profile);
    let issuer = Issuer::start(vec![(
        "/oauth/token",
        400,
        serde_json::json!({"error":"invalid_grant","token":current.refresh}).to_string(),
    )])
    .await;
    let error = session.prepare(&client(), &issuer.base, &Fixed(NOW)).await;
    assert!(matches!(error, Err(SignInError::Expired(_))));
    let rendered = format!("{error:?}");
    assert!(!rendered.contains(&current.access));
    assert!(!rendered.contains(&current.refresh));
    assert_eq!(
        load(&profile.0).unwrap_or_else(|error| panic!("{error}")),
        Some(current)
    );
}

#[test]
fn a_session_refuses_a_stored_account_that_differs_from_the_configured_account() {
    let profile = Profile::new("live-initial-account");
    save(&profile.0, &signed_in(NOW + HOUR)).unwrap_or_else(|error| panic!("{error}"));
    assert!(
        matches!(
            Session::open(&profile.0, "account-b"),
            Err(SignInError::AccountChanged)
        ),
        "a stored sign-in was bound to a different configured account"
    );
}

#[tokio::test]
async fn vaulted_connection_renews_one_bound_snapshot_and_refuses_foreign_refresh(
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::vault::{ApiDestination, DestinationBinding, OAuthRefreshDestination};
    let profile = Profile::new("bound-vault-renewal");
    crate::profile_security::protect_directory(&profile.0)?;
    let issuer = Issuer::start(vec![(
        "/oauth/token",
        200,
        token_answer("account-a", NOW + 72 * HOUR, Some("vault-rotated")),
    )])
    .await;
    let foreign = Issuer::start(Vec::new()).await;
    let files = SignInFiles::new("subscription.vault.oauth.json", "subscription.credential")?
        .with_vault(
            grapt_types::ProviderId::new("subscription").ok_or("connection")?,
            DestinationBinding::oauth(
                ApiDestination::parse("https://chatgpt.com")?,
                OAuthRefreshDestination::parse(&issuer.base)?,
            ),
        )?;
    let original = signed_in(NOW + HOUR);
    complete_sign_in_with_files(&profile.0, &files, &original, Timestamp::from_millis(NOW)).await?;
    let before = std::fs::read(profile.0.join(files.state_file()))?;
    let mut session =
        Session::open_with_files(&profile.0, "account-a", files.clone())?.ok_or("sign-in")?;
    assert!(session
        .prepare_startup(&client(), &foreign.base, &Fixed(NOW))
        .await
        .is_err());
    assert!(foreign.seen().is_empty());
    assert_eq!(std::fs::read(profile.0.join(files.state_file()))?, before);
    assert!(matches!(
        session
            .prepare_startup(&client(), &issuer.base, &Fixed(NOW))
            .await?,
        Renewal::Refreshed
    ));
    drop(session);
    let reopened = files.load(&profile.0)?.ok_or("rotated state")?;
    assert_eq!(reopened.refresh, "vault-rotated");
    assert_eq!(issuer.seen().len(), 1);
    assert!(!profile.0.join("subscription.credential").exists());
    let mut session = Session::open_with_files(&profile.0, "account-a", files.clone())?
        .ok_or("reopened sign-in")?;

    assert!(matches!(
        session
            .prepare(&client(), &issuer.base, &Fixed(NOW))
            .await?,
        Renewal::Current
    ));
    assert!(!profile.0.join("subscription.credential").exists());
    assert!(session
        .recover(&client(), &foreign.base, &Fixed(NOW), true)
        .await
        .is_err());
    assert!(foreign.seen().is_empty());
    assert_eq!(
        files.load(&profile.0)?.ok_or("retained rotation")?.refresh,
        "vault-rotated"
    );
    Ok(())
}
