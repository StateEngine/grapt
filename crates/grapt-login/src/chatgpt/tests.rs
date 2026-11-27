use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rustls_platform_verifier::BuilderVerifierExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

#[path = "tests/live.rs"]
mod live;
#[path = "tests/switch_recovery.rs"]
mod switch_recovery;

#[path = "tests/client_version.rs"]
mod client_version;

#[cfg(windows)]
#[path = "tests/protection.rs"]
mod protection;

const HOUR: i64 = 60 * 60 * 1000;
const NOW: i64 = 1_800_000_000_000;

#[derive(Debug)]
struct Fixed(i64);

impl ClockAuthority for Fixed {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

/// An unsigned JWT carrying `claims`.
fn jwt(claims: Value) -> String {
    let encode =
        |value: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string());
    format!(
        "{}.{}.signature",
        encode(&serde_json::json!({"alg": "none"})),
        encode(&claims)
    )
}

fn access(account: &str, expires_at: i64) -> String {
    jwt(serde_json::json!({
        "exp": expires_at / 1000,
        "https://api.openai.com/auth": {"chatgpt_account_id": account},
    }))
}

fn signed_in(expires_at: i64) -> Tokens {
    Tokens {
        access: access("account-a", expires_at),
        refresh: "refresh-1".to_owned(),
        expires_at: Timestamp::from_millis(expires_at),
        account: "account-a".to_owned(),
    }
}

/// A scratch profile directory, removed on drop.
struct Profile(PathBuf);

impl Profile {
    fn new(case: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("grapt-chatgpt-{case}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).unwrap_or_else(|error| panic!("profile: {error}"));
        Self(path)
    }

    fn read(&self, name: &str) -> String {
        let stored =
            std::fs::read(self.0.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
        let bytes = crate::profile_security::open(&stored, STATE_CAP)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        String::from_utf8(bytes).unwrap_or_else(|error| panic!("{name}: {error}"))
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One request the loopback issuer saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    body: String,
    head: String,
}

/// A loopback HTTP server that answers each request with the next scripted
/// `(path, status, body)`, refusing a request for any other path.
struct Issuer {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Issuer {
    async fn start(script: Vec<(&'static str, u16, String)>) -> Self {
        Self::start_delayed(script, Duration::ZERO).await
    }

    /// Like [`Self::start`], holding each answer back by `delay`.
    async fn start_delayed(script: Vec<(&'static str, u16, String)>, delay: Duration) -> Self {
        Self::start_controlled(script, delay, None).await
    }

    async fn start_held(
        script: Vec<(&'static str, u16, String)>,
    ) -> (Self, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let issuer = Self::start_controlled(
            script,
            Duration::ZERO,
            Some((entered.clone(), release.clone())),
        )
        .await;
        (issuer, entered, release)
    }

    async fn start_controlled(
        script: Vec<(&'static str, u16, String)>,
        delay: Duration,
        mut gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap_or_else(|error| panic!("bind: {error}"));
        let base = format!(
            "http://{}",
            listener
                .local_addr()
                .unwrap_or_else(|error| panic!("address: {error}"))
        );
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let mut script: VecDeque<_> = script.into();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                let (head, length) = loop {
                    let Ok(read) = stream.read(&mut buffer).await else {
                        return;
                    };
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..read]);
                    let text = String::from_utf8_lossy(&request).into_owned();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = text[..end]
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        break (end + 4, length);
                    }
                };
                while request.len() < head + length {
                    let Ok(read) = stream.read(&mut buffer).await else {
                        return;
                    };
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let path = text
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                let body = text[head.min(text.len())..].to_owned();
                if let Ok(mut seen) = record.lock() {
                    seen.push(Seen {
                        path: path.clone(),
                        body,
                        head: text[..head].to_owned(),
                    });
                }
                let (status, answer) = match script.pop_front() {
                    Some((expected, status, answer)) if expected == path => (status, answer),
                    _ => (500, "unscripted".to_owned()),
                };
                let answer = if path == "/models" && status == 200 {
                    client_version::filter_listing(&text[..head], answer)
                } else {
                    answer
                };
                if let Some((entered, release)) = gate.take() {
                    entered.notify_one();
                    tokio::time::timeout(Duration::from_secs(5), release.notified())
                        .await
                        .unwrap_or_else(|_| panic!("issuer response gate was not released"));
                }
                tokio::time::sleep(delay).await;
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self { base, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }
}

/// A loopback client without request timeouts: the paused-clock tests would
/// otherwise advance straight through them while waiting on the socket.
fn client() -> reqwest::Client {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(|builder| Ok(builder.with_platform_verifier()?.with_no_client_auth()))
        .unwrap_or_else(|error: rustls::Error| panic!("tls: {error}"));
    reqwest::Client::builder()
        .no_proxy()
        .use_preconfigured_tls(tls)
        .build()
        .unwrap_or_else(|error| panic!("client: {error}"))
}

fn token_answer(account: &str, expires_at: i64, refresh: Option<&str>) -> String {
    let mut answer = serde_json::json!({
        "access_token": access(account, expires_at),
        "id_token": jwt(serde_json::json!({"chatgpt_account_id": account})),
        "expires_in": 3600,
    });
    if let Some(refresh) = refresh {
        answer["refresh_token"] = Value::from(refresh);
    }
    answer.to_string()
}

#[test]
fn tokens_take_account_and_expiry_from_the_issued_tokens() {
    let answer: Value =
        serde_json::from_str(&token_answer("account-a", NOW + 240 * HOUR, Some("r")))
            .unwrap_or_else(|error| panic!("{error}"));
    let tokens = tokens(&answer, None, &Fixed(NOW)).unwrap_or_else(|| panic!("tokens"));
    assert_eq!(tokens.account, "account-a");
    // The access token's own exp claim wins over expires_in.
    assert_eq!(tokens.expires_at.as_millis(), NOW + 240 * HOUR);
    assert_eq!(tokens.refresh, "r");
}

#[test]
fn a_refresh_answer_without_a_new_refresh_token_keeps_the_old_one() {
    let answer: Value = serde_json::from_str(&token_answer("account-a", NOW + HOUR, None))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(tokens(&answer, None, &Fixed(NOW)).is_none());
    let kept = tokens(&answer, Some("old"), &Fixed(NOW)).unwrap_or_else(|| panic!("tokens"));
    assert_eq!(kept.refresh, "old");
}

#[test]
fn tokens_without_an_account_or_with_a_malformed_jwt_are_refused() {
    let answer = serde_json::json!({
        "access_token": jwt(serde_json::json!({"exp": 1})),
        "refresh_token": "r",
    });
    assert!(tokens(&answer, None, &Fixed(NOW)).is_none());
    for token in ["", "a.b", "a.b.c.d", "a.!!!.c"] {
        assert!(claims(token).is_none(), "{token}");
    }
}

#[test]
fn the_form_body_percent_encodes_reserved_bytes() {
    assert_eq!(
        form(&[("redirect_uri", "https://a/b?c=d e"), ("k", "~-._")]),
        "redirect_uri=https%3A%2F%2Fa%2Fb%3Fc%3Dd%20e&k=~-._"
    );
}

#[test]
fn debug_output_never_shows_a_token() {
    let tokens = signed_in(NOW);
    let shown = format!("{tokens:?}");
    assert!(
        !shown.contains(&tokens.access) && !shown.contains("refresh-1"),
        "{shown}"
    );
}

#[test]
fn saved_tokens_load_back_and_stay_private() {
    let profile = Profile::new("save");
    assert!(load(&profile.0)
        .unwrap_or_else(|error| panic!("{error}"))
        .is_none());
    let tokens = signed_in(NOW + HOUR);
    save(&profile.0, &tokens).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(profile.read(CREDENTIAL_FILE), tokens.access);
    assert_eq!(
        load(&profile.0).unwrap_or_else(|error| panic!("{error}")),
        Some(tokens)
    );
    #[cfg(unix)]
    for name in [CREDENTIAL_FILE, STATE_FILE] {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(profile.0.join(name))
            .map(|metadata| metadata.permissions().mode() & 0o777)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(mode, 0o600, "{name}");
    }
    // Only the two files remain: no temporary file is left behind.
    let names = std::fs::read_dir(&profile.0)
        .unwrap_or_else(|error| panic!("{error}"))
        .count();
    assert_eq!(names, 2);
}

#[tokio::test]
async fn a_profile_without_chatgpt_is_left_alone() {
    let profile = Profile::new("absent");
    let issuer = Issuer::start(Vec::new()).await;
    let renewal = refresh_if_due(&profile.0, &client(), &issuer.base, &Fixed(NOW)).await;
    assert!(matches!(renewal, Ok(Renewal::NotSignedIn)), "{renewal:?}");
    assert!(issuer.seen().is_empty());
}

#[tokio::test]
async fn a_token_far_from_expiry_is_not_refreshed() {
    let profile = Profile::new("current");
    save(&profile.0, &signed_in(NOW + 49 * HOUR)).unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(Vec::new()).await;
    let renewal = refresh_if_due(&profile.0, &client(), &issuer.base, &Fixed(NOW)).await;
    assert!(matches!(renewal, Ok(Renewal::Current)), "{renewal:?}");
    assert!(issuer.seen().is_empty());
}

#[tokio::test]
async fn a_token_near_expiry_is_refreshed_and_replaced() {
    let profile = Profile::new("due");
    save(&profile.0, &signed_in(NOW + 47 * HOUR)).unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(vec![(
        "/oauth/token",
        200,
        token_answer("account-a", NOW + 240 * HOUR, Some("refresh-2")),
    )])
    .await;
    let renewal = refresh_if_due(&profile.0, &client(), &issuer.base, &Fixed(NOW)).await;
    assert!(matches!(renewal, Ok(Renewal::Refreshed)), "{renewal:?}");
    let seen = issuer.seen();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].body.contains("grant_type=refresh_token"),
        "{:?}",
        seen[0]
    );
    assert!(
        seen[0].body.contains("refresh_token=refresh-1"),
        "{:?}",
        seen[0]
    );
    let stored = load(&profile.0)
        .unwrap_or_else(|error| panic!("{error}"))
        .unwrap_or_else(|| panic!("tokens"));
    assert_eq!(stored.refresh, "refresh-2");
    assert_eq!(stored.expires_at.as_millis(), NOW + 240 * HOUR);
    assert_eq!(
        profile.read(CREDENTIAL_FILE),
        access("account-a", NOW + 240 * HOUR)
    );
}

#[tokio::test]
async fn a_failed_refresh_of_a_still_valid_token_defers_and_keeps_it() {
    let profile = Profile::new("deferred");
    let current = signed_in(NOW + HOUR);
    save(&profile.0, &current).unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(vec![("/oauth/token", 401, String::new())]).await;
    let renewal = refresh_if_due(&profile.0, &client(), &issuer.base, &Fixed(NOW)).await;
    assert!(
        matches!(
            renewal,
            Ok(Renewal::Deferred(SignInError::Refused { status: 401, .. }))
        ),
        "{renewal:?}"
    );
    assert_eq!(
        load(&profile.0).unwrap_or_else(|error| panic!("{error}")),
        Some(current)
    );
}

#[tokio::test]
async fn an_expired_token_that_cannot_be_refreshed_stops_the_launch() {
    let profile = Profile::new("expired");
    let current = signed_in(NOW - HOUR);
    save(&profile.0, &current).unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(vec![("/oauth/token", 400, String::new())]).await;
    let renewal = refresh_if_due(&profile.0, &client(), &issuer.base, &Fixed(NOW)).await;
    let Err(error) = renewal else {
        panic!("an expired sign-in must stop the launch: {renewal:?}");
    };
    assert!(error.to_string().contains("sign in again"), "{error}");
    assert_eq!(
        load(&profile.0).unwrap_or_else(|error| panic!("{error}")),
        Some(current)
    );
}

#[tokio::test]
async fn a_refresh_that_changes_account_is_refused() {
    let profile = Profile::new("account");
    let current = signed_in(NOW + HOUR);
    save(&profile.0, &current).unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(vec![(
        "/oauth/token",
        200,
        token_answer("account-b", NOW + 240 * HOUR, Some("refresh-2")),
    )])
    .await;
    let renewal = refresh_if_due(&profile.0, &client(), &issuer.base, &Fixed(NOW)).await;
    assert!(
        matches!(renewal, Ok(Renewal::Deferred(SignInError::AccountChanged))),
        "{renewal:?}"
    );
    assert_eq!(
        load(&profile.0).unwrap_or_else(|error| panic!("{error}")),
        Some(current)
    );
}

#[tokio::test(start_paused = true)]
async fn the_device_flow_polls_until_approval_then_exchanges_the_code() {
    let issuer = Issuer::start(vec![
        (
            "/api/accounts/deviceauth/usercode",
            200,
            serde_json::json!({"device_auth_id": "device-1", "user_code": "ABCD-1234", "interval": "1"})
                .to_string(),
        ),
        ("/api/accounts/deviceauth/token", 403, String::new()),
        ("/api/accounts/deviceauth/token", 404, String::new()),
        (
            "/api/accounts/deviceauth/token",
            200,
            serde_json::json!({"authorization_code": "code-1", "code_verifier": "verifier-1"})
                .to_string(),
        ),
        (
            "/oauth/token",
            200,
            token_answer("account-a", NOW + 240 * HOUR, Some("refresh-1")),
        ),
    ])
    .await;
    let client = client();
    let code = request_code(&client, &issuer.base)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(code.user_code, "ABCD-1234");
    assert_eq!(
        code.verification_url,
        format!("{}/codex/device", issuer.base)
    );
    let tokens = wait_for_approval(&client, &issuer.base, &code, &Fixed(NOW))
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(tokens.account, "account-a");
    assert_eq!(tokens.refresh, "refresh-1");
    let seen = issuer.seen();
    let paths: Vec<_> = seen.iter().map(|seen| seen.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/api/accounts/deviceauth/usercode",
            "/api/accounts/deviceauth/token",
            "/api/accounts/deviceauth/token",
            "/api/accounts/deviceauth/token",
            "/oauth/token",
        ]
    );
    assert!(
        seen[1].body.contains("\"device_auth_id\":\"device-1\""),
        "{:?}",
        seen[1]
    );
    let exchange = &seen[4].body;
    assert!(
        exchange.contains("grant_type=authorization_code"),
        "{exchange}"
    );
    assert!(exchange.contains("code=code-1"), "{exchange}");
    assert!(exchange.contains("code_verifier=verifier-1"), "{exchange}");
    assert!(
        exchange.contains("redirect_uri=http%3A%2F%2F127.0.0.1"),
        "{exchange}"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unapproved_code_times_out_after_its_allowance() {
    let pending: Vec<_> = (0..400)
        .map(|_| ("/api/accounts/deviceauth/token", 403, String::new()))
        .collect();
    let issuer = Issuer::start(pending).await;
    let code = DeviceCode {
        verification_url: String::new(),
        user_code: "ABCD".to_owned(),
        device_auth_id: "device-1".to_owned(),
        interval: Duration::from_secs(5),
    };
    let result = wait_for_approval(&client(), &issuer.base, &code, &Fixed(NOW)).await;
    assert!(matches!(result, Err(SignInError::TimedOut)), "{result:?}");
    // 15 minutes at one poll per 8 seconds.
    assert!(
        (100..=115).contains(&issuer.seen().len()),
        "{}",
        issuer.seen().len()
    );
}

#[tokio::test(start_paused = true)]
async fn a_denied_code_stops_polling() {
    let issuer = Issuer::start(vec![("/api/accounts/deviceauth/token", 410, String::new())]).await;
    let code = DeviceCode {
        verification_url: String::new(),
        user_code: "ABCD".to_owned(),
        device_auth_id: "device-1".to_owned(),
        interval: Duration::from_secs(5),
    };
    let result = wait_for_approval(&client(), &issuer.base, &code, &Fixed(NOW)).await;
    assert!(
        matches!(result, Err(SignInError::Refused { status: 410, .. })),
        "{result:?}"
    );
}

#[tokio::test]
async fn discovery_returns_the_account_listing_and_refuses_anything_else() {
    let listing = serde_json::json!({"models": [{"slug": "gpt-5.5", "visibility": "list", "context_window": 272000}]})
        .to_string();
    let issuer = Issuer::start(vec![
        ("/models", 200, listing.clone()),
        ("/models", 200, "[]".to_owned()),
        ("/models", 401, String::new()),
    ])
    .await;
    let client = client();
    let tokens = signed_in(NOW + HOUR);
    let bytes = discover_models(&client, &issuer.base, &tokens)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(bytes, listing.as_bytes());
    assert!(matches!(
        discover_models(&client, &issuer.base, &tokens).await,
        Err(SignInError::Malformed(_))
    ));
    assert!(matches!(
        discover_models(&client, &issuer.base, &tokens).await,
        Err(SignInError::Refused { status: 401, .. })
    ));
}

fn temporary(directory: &std::path::Path, name: &str) -> PathBuf {
    directory.join(format!(".{name}.{}.new", std::process::id()))
}

#[test]
fn an_interrupted_save_keeps_the_previous_generation_whole() {
    let profile = Profile::new("interrupted");
    let first = signed_in(NOW + HOUR);
    save(&profile.0, &first).unwrap_or_else(|error| panic!("{error}"));
    // Something already holds this save's temporary path, so it cannot write.
    let blocker = temporary(&profile.0, STATE_FILE);
    std::fs::create_dir(&blocker).unwrap_or_else(|error| panic!("{error}"));
    let second = Tokens {
        access: access("account-a", NOW + 240 * HOUR),
        refresh: "refresh-2".to_owned(),
        expires_at: Timestamp::from_millis(NOW + 240 * HOUR),
        account: "account-a".to_owned(),
    };
    assert!(save(&profile.0, &second).is_err());
    assert_eq!(
        load(&profile.0).unwrap_or_else(|error| panic!("{error}")),
        Some(first.clone())
    );
    assert_eq!(profile.read(CREDENTIAL_FILE), first.access);
    // A temporary this save did not create is left alone.
    assert!(blocker.is_dir());
}

#[tokio::test]
async fn a_start_after_an_interrupted_projection_derives_the_credential_again() {
    let profile = Profile::new("projection");
    let first = signed_in(NOW + HOUR);
    save(&profile.0, &first).unwrap_or_else(|error| panic!("{error}"));
    // Interrupted after the new generation was stored, before its projection.
    let second = Tokens {
        access: access("account-a", NOW + 240 * HOUR),
        refresh: "refresh-2".to_owned(),
        expires_at: Timestamp::from_millis(NOW + 240 * HOUR),
        account: "account-a".to_owned(),
    };
    replace(&profile.0, STATE_FILE, &encode(&second)).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(profile.read(CREDENTIAL_FILE), first.access);
    let issuer = Issuer::start(Vec::new()).await;
    let renewal = refresh_if_due(&profile.0, &client(), &issuer.base, &Fixed(NOW)).await;
    assert!(matches!(renewal, Ok(Renewal::Current)), "{renewal:?}");
    assert_eq!(profile.read(CREDENTIAL_FILE), second.access);
    assert!(issuer.seen().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_starts_submit_a_refresh_token_once() {
    let profile = Profile::new("concurrent");
    save(&profile.0, &signed_in(NOW + HOUR)).unwrap_or_else(|error| panic!("{error}"));
    // The issuer rotates the refresh token and refuses its reuse. Each answer
    // is held back so both starts are in flight together.
    let issuer = Issuer::start_delayed(
        vec![
            (
                "/oauth/token",
                200,
                token_answer("account-a", NOW + 240 * HOUR, Some("refresh-2")),
            ),
            ("/oauth/token", 400, "reused refresh token".to_owned()),
        ],
        Duration::from_millis(300),
    )
    .await;
    let client = client();
    let (first, second) = tokio::join!(
        refresh_if_due(&profile.0, &client, &issuer.base, &Fixed(NOW)),
        refresh_if_due(&profile.0, &client, &issuer.base, &Fixed(NOW)),
    );
    let outcomes = [first, second];
    assert!(
        outcomes
            .iter()
            .any(|outcome| matches!(outcome, Ok(Renewal::Refreshed)))
            && outcomes
                .iter()
                .any(|outcome| matches!(outcome, Ok(Renewal::Current))),
        "{outcomes:?}"
    );
    let submitted = issuer
        .seen()
        .iter()
        .filter(|seen| seen.body.contains("refresh_token=refresh-1"))
        .count();
    assert_eq!(submitted, 1);
    let stored = load(&profile.0)
        .unwrap_or_else(|error| panic!("{error}"))
        .unwrap_or_else(|| panic!("tokens"));
    assert_eq!(stored.refresh, "refresh-2");
}

#[test]
fn oversized_sign_in_files_are_refused_without_reading_them_whole() {
    let profile = Profile::new("oversized");
    let state = profile.0.join(STATE_FILE);
    // A sparse 128 MiB file: reading it whole would allocate 128 MiB.
    std::fs::File::create(&state)
        .and_then(|file| file.set_len(128 * 1024 * 1024))
        .unwrap_or_else(|error| panic!("{error}"));
    let Err(error) = load(&profile.0) else {
        panic!("an oversized state file must be refused");
    };
    assert!(error.to_string().contains("too large"), "{error}");
    // At the cap exactly it is read; one byte more is refused.
    std::fs::write(&state, vec![b' '; STATE_CAP]).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        read_bounded(&state, STATE_CAP)
            .unwrap_or_else(|error| panic!("{error}"))
            .map(|bytes| bytes.len()),
        Some(STATE_CAP)
    );
    std::fs::write(&state, vec![b' '; STATE_CAP + 1]).unwrap_or_else(|error| panic!("{error}"));
    assert!(read_bounded(&state, STATE_CAP).is_err());
}

#[test]
fn an_oversized_or_foreign_credential_is_replaced_by_the_generation() {
    let profile = Profile::new("projection-oversized");
    let tokens = signed_in(NOW + HOUR);
    save(&profile.0, &tokens).unwrap_or_else(|error| panic!("{error}"));
    std::fs::write(profile.0.join(CREDENTIAL_FILE), vec![b'x'; TOKEN_MAX + 1])
        .unwrap_or_else(|error| panic!("{error}"));
    project(&profile.0, &tokens).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(profile.read(CREDENTIAL_FILE), tokens.access);
}

fn write_config(profile: &std::path::Path, credential_version: &str) {
    let config =
        serde_json::json!({"provider": {"id": "openai", "credential_version": credential_version}});
    std::fs::write(profile.join("grapt.jsonc"), config.to_string())
        .unwrap_or_else(|error| panic!("{error}"));
}

#[tokio::test]
async fn an_interrupted_first_sign_in_is_refused_with_a_way_out() {
    // The import created the profile (with its access token), and the start
    // was interrupted before the sign-in was stored: the token would work
    // until it expired and then could never be renewed.
    let profile = Profile::new("unfinished");
    write_config(&profile.0, SIGN_IN_CREDENTIAL_VERSION);
    std::fs::write(profile.0.join(CREDENTIAL_FILE), "access-a")
        .unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(Vec::new()).await;
    let renewal = refresh_if_due(&profile.0, &client(), &issuer.base, &Fixed(NOW)).await;
    let Err(error) = renewal else {
        panic!("an unfinished sign-in must stop the launch: {renewal:?}");
    };
    assert!(error.to_string().contains("sign in again"), "{error}");
    assert_eq!(profile.read(CREDENTIAL_FILE), "access-a");
    assert!(issuer.seen().is_empty());
}

#[tokio::test]
async fn a_profile_imported_from_another_account_is_left_alone() {
    let profile = Profile::new("imported");
    write_config(&profile.0, "imported-account-v1");
    std::fs::write(profile.0.join(CREDENTIAL_FILE), "account-b-key")
        .unwrap_or_else(|error| panic!("{error}"));
    let issuer = Issuer::start(Vec::new()).await;
    let renewal = refresh_if_due(&profile.0, &client(), &issuer.base, &Fixed(NOW)).await;
    assert!(matches!(renewal, Ok(Renewal::NotSignedIn)), "{renewal:?}");
    assert_eq!(profile.read(CREDENTIAL_FILE), "account-b-key");
    assert!(!profile.0.join(STATE_FILE).exists() && !profile.0.join(LOCK_FILE).exists());
}

#[tokio::test]
async fn completing_a_first_sign_in_stores_the_generation_and_its_credential() {
    let profile = Profile::new("complete");
    crate::profile_security::protect_directory(&profile.0)
        .unwrap_or_else(|error| panic!("private first-sign-in fixture: {error}"));
    write_config(&profile.0, SIGN_IN_CREDENTIAL_VERSION);
    let tokens = signed_in(NOW + 240 * HOUR);
    complete_sign_in(&profile.0, &tokens)
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        load(&profile.0).unwrap_or_else(|error| panic!("{error}")),
        Some(tokens.clone())
    );
    assert_eq!(profile.read(CREDENTIAL_FILE), tokens.access);
    let issuer = Issuer::start(Vec::new()).await;
    let renewal = refresh_if_due(&profile.0, &client(), &issuer.base, &Fixed(NOW)).await;
    assert!(matches!(renewal, Ok(Renewal::Current)), "{renewal:?}");
}

#[cfg(unix)]
#[test]
fn a_fifo_in_place_of_a_sign_in_file_is_refused_without_blocking() {
    let profile = Profile::new("fifo");
    for name in [STATE_FILE, "grapt.jsonc"] {
        let path = profile.0.join(name);
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap_or_else(|error| panic!("mkfifo: {error}"));
        assert!(made.success(), "mkfifo {name}");
        // Nothing ever writes to the FIFO: a blocking open would never return.
        let (sender, outcome) = std::sync::mpsc::sync_channel(1);
        let reading = path.clone();
        std::thread::spawn(move || {
            let _ = sender.send(read_bounded(&reading, STATE_CAP).map(|_| ()));
        });
        let result = outcome
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|_| panic!("reading a FIFO {name} blocked"));
        let Err(error) = result else {
            panic!("a FIFO {name} must be refused");
        };
        assert!(error.to_string().contains("not a regular file"), "{error}");
        std::fs::remove_file(&path).unwrap_or_else(|error| panic!("{error}"));
    }
}
