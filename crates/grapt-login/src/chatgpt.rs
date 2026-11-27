//! Sign in with ChatGPT: the OAuth device-code flow against auth.openai.com,
//! the account's model discovery, and its serialized credential renewal.
//!
//! Startup refresh uses [`REFRESH_AHEAD`]; the token's own expiry determines
//! when it needs renewal.
//! [`STATE_FILE`] is the one authoritative sign-in generation (access and
//! refresh token, expiry, account); `provider.credential` is derived from it
//! when the sign-in owner starts or prepares a request. Both are replaced, never
//! rewritten in place, and renewal is serialized per profile.

use std::fmt;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use base64::Engine;
pub use grapt_types::CODEX_COMPATIBILITY_VERSION;
use grapt_types::{ClockAuthority, Timestamp};
use serde_json::Value;

mod browser;
pub use browser::BrowserSignIn;

mod live;
pub use live::{Session, SignInLockAuthority};
mod files;
pub use files::{complete_sign_in_with_files, SignInFileLock, SignInFiles};
mod client;
pub(crate) use client::bounded_client;
#[cfg(feature = "loopback-testkit")]
pub use client::loopback_client;
pub use client::production_client;
pub use reqwest::Client as SignInClient;

/// The OAuth issuer.
pub const ISSUER: &str = "https://auth.openai.com";

/// The ChatGPT plan's Codex backend, which serves the account's models.
pub const CODEX_BACKEND: &str = "https://chatgpt.com/backend-api/codex";

/// The public OAuth client of OpenAI's Codex CLI, whose ChatGPT route the
/// product's `chatgpt-codex` protocol family speaks.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Refresh at launch when the access token expires sooner than this.
pub const REFRESH_AHEAD: Duration = Duration::from_secs(48 * 60 * 60);

/// How long a person has to approve the device code. OpenAI's codes expire
/// after fifteen minutes.
const SIGN_IN_ALLOWANCE: Duration = Duration::from_secs(15 * 60);

/// Added to the issuer's polling interval, as its other clients do.
const POLL_MARGIN: Duration = Duration::from_secs(3);

/// The most bytes any issuer or discovery answer may carry. A discovery
/// listing of the account's models is the largest.
const RESPONSE_CAP: usize = 4 * 1024 * 1024;

/// The profile file that carries the refresh token, expiry and account id.
pub const STATE_FILE: &str = "chatgpt.oauth.json";

/// The profile file the product reads its access token from.
pub const CREDENTIAL_FILE: &str = "provider.credential";

/// The profile file whose lock serializes renewal between starts.
const LOCK_FILE: &str = "chatgpt.lock";

/// How long an owner waits for another one renewing the same sign-in: longer
/// than one bounded token request.
const LOCK_ALLOWANCE: Duration = Duration::from_secs(60);

const LOCK_POLL: Duration = Duration::from_millis(100);

/// The most bytes a stored token may have, and the sign-in state file.
const TOKEN_MAX: usize = 4096;
const STATE_CAP: usize = 16 * 1024;

/// The largest profile configuration read, the importer's own bound.
const CONFIG_CAP: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error("could not reach {0}; check the network and try again")]
    Request(&'static str),
    #[error("{what} answered HTTP {status}")]
    Refused { what: &'static str, status: u16 },
    #[error("{0} answered with an unusable response")]
    Malformed(&'static str),
    #[error("the sign-in code was not approved within 15 minutes; start again")]
    TimedOut,
    #[error("the ChatGPT sign-in changed account; restart Grapt to use the new account")]
    AccountChanged,
    #[error("the ChatGPT profile directory changed; restart with the intended profile")]
    ProfileChanged,
    #[error("the ChatGPT sign-in has expired and could not be renewed ({0}); sign in again with grapt login chatgpt --profile <profile>")]
    Expired(Box<SignInError>),
    #[error("this profile's ChatGPT sign-in did not finish; move the profile aside and run grapt code to sign in again")]
    Unfinished,
    #[error("profile sign-in state {0}")]
    Store(String),
}

/// A device code waiting for a person to approve it in a browser.
#[derive(Debug)]
pub struct DeviceCode {
    /// Open this page...
    pub verification_url: String,
    /// ...and enter this code.
    pub user_code: String,
    device_auth_id: String,
    interval: Duration,
}

/// A signed-in account's tokens. `Debug` never shows a token.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    pub expires_at: Timestamp,
    pub account: String,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tokens")
            .field("expires_at", &self.expires_at)
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// Ask the issuer for a device code.
pub async fn request_code(
    client: &reqwest::Client,
    issuer: &str,
) -> Result<DeviceCode, SignInError> {
    const WHAT: &str = "the ChatGPT sign-in service";
    let body = serde_json::json!({ "client_id": CLIENT_ID }).to_string();
    let (status, answer) = post(
        client,
        WHAT,
        &format!("{issuer}/api/accounts/deviceauth/usercode"),
        "application/json",
        body,
    )
    .await?;
    if status != 200 {
        return Err(SignInError::Refused { what: WHAT, status });
    }
    let text = |field: &str| {
        answer
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= 1024)
            .map(str::to_owned)
            .ok_or(SignInError::Malformed(WHAT))
    };
    // The interval arrives as a string from this issuer; accept a number too.
    let seconds = match answer.get("interval") {
        Some(Value::String(text)) => text.parse().ok(),
        Some(Value::Number(number)) => number.as_u64(),
        _ => None,
    };
    Ok(DeviceCode {
        verification_url: format!("{issuer}/codex/device"),
        user_code: text("user_code")?,
        device_auth_id: text("device_auth_id")?,
        interval: Duration::from_secs(seconds.unwrap_or(5).clamp(1, 60)),
    })
}

/// Poll until the person approves `code`, then exchange it for tokens.
pub async fn wait_for_approval(
    client: &reqwest::Client,
    issuer: &str,
    code: &DeviceCode,
    clock: &(dyn ClockAuthority + Sync),
) -> Result<Tokens, SignInError> {
    const WHAT: &str = "the ChatGPT sign-in service";
    let body = serde_json::json!({
        "device_auth_id": code.device_auth_id,
        "user_code": code.user_code,
    })
    .to_string();
    let deadline = tokio::time::Instant::now() + SIGN_IN_ALLOWANCE;
    let approval = loop {
        let (status, answer) = post(
            client,
            WHAT,
            &format!("{issuer}/api/accounts/deviceauth/token"),
            "application/json",
            body.clone(),
        )
        .await?;
        match status {
            200 => break answer,
            // Not approved yet.
            403 | 404 => {}
            status => return Err(SignInError::Refused { what: WHAT, status }),
        }
        let next = tokio::time::Instant::now() + code.interval + POLL_MARGIN;
        if next >= deadline {
            return Err(SignInError::TimedOut);
        }
        tokio::time::sleep_until(next).await;
    };
    let field = |name: &str| {
        approval
            .get(name)
            .and_then(Value::as_str)
            .ok_or(SignInError::Malformed(WHAT))
    };
    let form = form(&[
        ("grant_type", "authorization_code"),
        ("code", field("authorization_code")?),
        ("redirect_uri", &format!("{issuer}/deviceauth/callback")),
        ("client_id", CLIENT_ID),
        ("code_verifier", field("code_verifier")?),
    ]);
    token_request(client, issuer, form, None, clock).await
}

/// Exchange a refresh token for fresh tokens on the same account.
pub async fn refresh(
    client: &reqwest::Client,
    issuer: &str,
    current: &Tokens,
    clock: &(dyn ClockAuthority + Sync),
) -> Result<Tokens, SignInError> {
    let form = form(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", &current.refresh),
        ("client_id", CLIENT_ID),
    ]);
    let renewed = token_request(client, issuer, form, Some(&current.refresh), clock).await?;
    if renewed.account != current.account {
        return Err(SignInError::AccountChanged);
    }
    if renewed.expires_at.as_millis() <= clock.now().as_millis() {
        return Err(SignInError::Malformed("the ChatGPT token renewal"));
    }
    Ok(renewed)
}

async fn token_request(
    client: &reqwest::Client,
    issuer: &str,
    form: String,
    previous_refresh: Option<&str>,
    clock: &(dyn ClockAuthority + Sync),
) -> Result<Tokens, SignInError> {
    const WHAT: &str = "the ChatGPT token service";
    let (status, answer) = post(
        client,
        WHAT,
        &format!("{issuer}/oauth/token"),
        "application/x-www-form-urlencoded",
        form,
    )
    .await?;
    if status != 200 {
        return Err(SignInError::Refused { what: WHAT, status });
    }
    tokens(&answer, previous_refresh, clock).ok_or(SignInError::Malformed(WHAT))
}

/// Read the tokens out of a token-endpoint answer. A refresh answer may omit
/// a new refresh token, in which case the previous one stays valid.
fn tokens(
    answer: &Value,
    previous_refresh: Option<&str>,
    clock: &(dyn ClockAuthority + Sync),
) -> Option<Tokens> {
    let text = |name: &str| {
        answer
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let access = text("access_token")?;
    let refresh = text("refresh_token").or(previous_refresh)?;
    let access_claims = claims(access);
    let account = text("id_token")
        .and_then(claims)
        .as_ref()
        .and_then(account_id)
        .or_else(|| access_claims.as_ref().and_then(account_id))?;
    // The token's own expiry wins over the answer's advisory lifetime.
    let expires_at = match access_claims
        .as_ref()
        .and_then(|claims| claims.get("exp")?.as_i64())
    {
        Some(seconds) => seconds.checked_mul(1000)?,
        None => {
            let lifetime = answer
                .get("expires_in")
                .and_then(Value::as_i64)
                .unwrap_or(3600);
            clock
                .now()
                .as_millis()
                .checked_add(lifetime.checked_mul(1000)?)?
        }
    };
    if access.len() > TOKEN_MAX || refresh.len() > TOKEN_MAX || account.len() > 128 {
        return None;
    }
    Some(Tokens {
        access: access.to_owned(),
        refresh: refresh.to_owned(),
        expires_at: Timestamp::from_millis(expires_at),
        account,
    })
}

/// The unverified claims of a JWT. They are read only to learn which account
/// and expiry the issuer stated; the token itself is checked by its audience.
fn claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let (_, payload, _, None) = (parts.next()?, parts.next()?, parts.next()?, parts.next()) else {
        return None;
    };
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn account_id(claims: &Value) -> Option<String> {
    claims
        .get("chatgpt_account_id")
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")?
                .get("chatgpt_account_id")
        })
        .or_else(|| claims.get("organizations")?.get(0)?.get("id"))
        .and_then(Value::as_str)
        .filter(|account| !account.is_empty())
        .map(str::to_owned)
}

/// The account's model listing, in the form the product reads as its
/// discovery snapshot (`{"models":[...]}`).
pub async fn discover_models(
    client: &reqwest::Client,
    backend: &str,
    tokens: &Tokens,
) -> Result<Vec<u8>, SignInError> {
    const WHAT: &str = "ChatGPT model discovery";
    let response = client
        .get(format!(
            "{backend}/models?client_version={}",
            CODEX_COMPATIBILITY_VERSION
        ))
        .header("authorization", format!("Bearer {}", tokens.access))
        .header("chatgpt-account-id", &tokens.account)
        .header("originator", ORIGINATOR)
        .header("version", CODEX_COMPATIBILITY_VERSION)
        .header("user-agent", concat!("grapt/", env!("CARGO_PKG_VERSION")))
        .send()
        .await
        .map_err(|_| SignInError::Request(WHAT))?;
    let status = response.status().as_u16();
    let bytes = bounded(response, WHAT).await?;
    if status != 200 {
        return Err(SignInError::Refused { what: WHAT, status });
    }
    let listing: Value =
        serde_json::from_slice(&bytes).map_err(|_| SignInError::Malformed(WHAT))?;
    if !listing.get("models").is_some_and(Value::is_array) {
        return Err(SignInError::Malformed(WHAT));
    }
    Ok(bytes)
}

async fn post(
    client: &reqwest::Client,
    what: &'static str,
    url: &str,
    content_type: &str,
    body: String,
) -> Result<(u16, Value), SignInError> {
    let response = client
        .post(url)
        .header("content-type", content_type)
        .header("originator", ORIGINATOR)
        .body(body)
        .send()
        .await
        .map_err(|_| SignInError::Request(what))?;
    let status = response.status().as_u16();
    let bytes = bounded(response, what).await?;
    // Refusals need not carry JSON; only a success has to.
    let answer = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status == 200 && !answer.is_object() {
        return Err(SignInError::Malformed(what));
    }
    Ok((status, answer))
}

async fn bounded(
    mut response: reqwest::Response,
    what: &'static str,
) -> Result<Vec<u8>, SignInError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| SignInError::Request(what))?
    {
        if bytes.len() + chunk.len() > RESPONSE_CAP {
            return Err(SignInError::Malformed(what));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// `application/x-www-form-urlencoded`, percent-encoding everything outside
/// the unreserved set.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    let encode = |text: &str| {
        text.bytes()
            .map(|byte| match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    char::from(byte).to_string()
                }
                byte => format!("%{byte:02X}"),
            })
            .collect::<String>()
    };
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Store `tokens` as the profile's sign-in generation, then derive the
/// product's credential from it.
pub fn save(profile: &Path, tokens: &Tokens) -> Result<(), SignInError> {
    replace(profile, STATE_FILE, &encode(tokens))?;
    project(profile, tokens)
}

/// The profile's sign-in generation, or `None` when it is not signed in to
/// ChatGPT. Only [`STATE_FILE`] is read: `provider.credential` is derived.
pub fn load(profile: &Path) -> Result<Option<Tokens>, SignInError> {
    read_bounded(&profile.join(STATE_FILE), STATE_CAP)?
        .map(|bytes| decode(&bytes))
        .transpose()
}

fn encode(tokens: &Tokens) -> Vec<u8> {
    serde_json::json!({
        "schema": "secode.chatgpt-oauth.v2",
        "access_token": tokens.access,
        "refresh_token": tokens.refresh,
        "expires_at_ms": tokens.expires_at.as_millis(),
        "account": tokens.account,
    })
    .to_string()
    .into_bytes()
}

fn decode(bytes: &[u8]) -> Result<Tokens, SignInError> {
    let malformed = || SignInError::Store(format!("{STATE_FILE} is damaged"));
    let state: Value = serde_json::from_slice(bytes).map_err(|_| malformed())?;
    if state.get("schema").and_then(Value::as_str) != Some("secode.chatgpt-oauth.v2") {
        return Err(malformed());
    }
    let text = |name: &str| {
        state
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.len() <= TOKEN_MAX)
            .map(str::to_owned)
            .ok_or_else(malformed)
    };
    Ok(Tokens {
        access: text("access_token")?,
        refresh: text("refresh_token")?,
        expires_at: Timestamp::from_millis(
            state
                .get("expires_at_ms")
                .and_then(Value::as_i64)
                .ok_or_else(malformed)?,
        ),
        account: text("account")?,
    })
}

/// Make `provider.credential` carry the generation's access token. The
/// file is replaced only when it differs from the authoritative generation.
fn project(profile: &Path, tokens: &Tokens) -> Result<(), SignInError> {
    // A legacy plaintext projection must be replaced even when its value matches.
    #[cfg(windows)]
    let already_protected = migration::projection_is_protected(profile);
    match read_bounded(&profile.join(CREDENTIAL_FILE), TOKEN_MAX) {
        Ok(Some(bytes))
            if bytes == tokens.access.as_bytes() && {
                #[cfg(windows)]
                {
                    already_protected
                }
                #[cfg(not(windows))]
                {
                    true
                }
            } =>
        {
            Ok(())
        }
        // Missing, different, oversized or unreadable: the generation wins.
        _ => replace(profile, CREDENTIAL_FILE, tokens.access.as_bytes()),
    }
}

/// The credential version a first sign-in writes into the account
/// configuration it imports. A profile that carries it but has no
/// [`STATE_FILE`] is one whose first sign-in was interrupted after the
/// profile was created and before its sign-in was stored.
/// The `originator` OpenAI's issuer and Codex backend see. Held at its
/// pre-rename value until they are verified to accept a new one.
pub(crate) const ORIGINATOR: &str = "secode"; // grapt-rename: keep
pub const SIGN_IN_CREDENTIAL_VERSION: &str = "chatgpt-oauth-v1";

/// Finish a first sign-in once its profile exists: store the generation
/// under the profile's lock. Nothing about the sign-in is persisted before
/// this, so an interrupted or refused setup leaves no sign-in state behind.
pub async fn complete_sign_in(profile: &Path, tokens: &Tokens) -> Result<(), SignInError> {
    let _protection = crate::profile_security::ProtectionBatch::begin()
        .map_err(|error| write_failure("profile protection", "started", &error))?;
    let _owner = own(profile).await?;
    #[cfg(windows)]
    migration::recover(profile).map_err(migration_error)?;
    save(profile, tokens)
}

/// A pending sign-in switch blocks other connection writers until its owner
/// finishes recovery. Reads keep their already captured generation.
pub fn has_pending_switch(profile: &Path) -> std::io::Result<bool> {
    switch::pending(profile)
}

/// Finish a committed switch before a startup pins its account. The existing
/// sign-in owner serializes recovery; no provider request is made here.
pub async fn recover_pending_switch(profile: &Path) -> Result<bool, SignInError> {
    if !has_pending_switch(profile)
        .map_err(|error| write_failure("sign-in switch", "probe", &error))?
    {
        return Ok(false);
    }
    let _owner = own(profile).await?;
    Ok(true)
}

/// Replace an existing profile's sign-in in place (`grapt login chatgpt`).
///
/// Under the profile's sign-in lock — the one a running service takes before
/// every request — `prepare` reads the profile and returns the validated
/// files that describe the new account. Those files and the new generation
/// are then published as one recoverable switch (see [`SwitchFile`]): a
/// failure or crash leaves either the old generation or the new one, never a
/// profile naming one account while signed in to another.
pub async fn replace_sign_in<T, E: From<SignInError>>(
    profile: &Path,
    tokens: &Tokens,
    prepare: impl FnOnce(&Path) -> Result<(T, Vec<SwitchFile>), E>,
) -> Result<T, E> {
    let _protection = crate::profile_security::ProtectionBatch::begin()
        .map_err(|error| write_failure("profile protection", "started", &error))?;
    let _owner = own(profile).await?;
    #[cfg(windows)]
    migration::recover(profile).map_err(migration_error)?;
    let (prepared, files) = prepare(profile)?;
    switch::switch(profile, tokens, &files)?;
    Ok(prepared)
}

/// Whether the profile's configuration came from a first sign-in that never
/// stored its generation.
fn unfinished_sign_in(profile: &Path) -> Result<bool, SignInError> {
    let marker = SIGN_IN_CREDENTIAL_VERSION.as_bytes();
    let config = crate::profile_files::profile_spelling(profile)
        .map_err(|error| {
            SignInError::Store(format!(
                "profile configuration is unreadable: {:?}",
                error.kind()
            ))
        })?
        .spelling
        .config_jsonc();
    Ok(read_bounded(&profile.join(config), CONFIG_CAP)?
        .is_some_and(|config| config.windows(marker.len()).any(|window| window == marker)))
}

/// Whether this profile needs the sign-in owner, including interrupted setup.
pub fn requires_renewal(profile: &Path) -> Result<bool, SignInError> {
    Ok(
        read_bounded(&profile.join(STATE_FILE), STATE_CAP)?.is_some()
            || unfinished_sign_in(profile)?,
    )
}

/// What [`refresh_if_due`] did.
#[derive(Debug)]
pub enum Renewal {
    /// The profile is not signed in to ChatGPT.
    NotSignedIn,
    /// The access token has more than [`REFRESH_AHEAD`] left.
    Current,
    /// The access token was renewed.
    Refreshed,
    /// Renewal failed but the access token still works for now.
    Deferred(SignInError),
}

/// Bring the profile's ChatGPT sign-in up to date before the product starts:
/// renew an access token that expires within [`REFRESH_AHEAD`] and derive
/// `provider.credential` from the result. Fails when the token has already
/// expired and cannot be renewed, when a first sign-in never finished, or
/// when the sign-in state cannot be read or written.
///
/// One process at a time owns this for a profile: the generation is re-read
/// after taking the profile's lock, and the lock is held through persistence,
/// so a refresh token is never submitted twice.
pub async fn refresh_if_due(
    profile: &Path,
    client: &reqwest::Client,
    issuer: &str,
    clock: &(dyn ClockAuthority + Sync),
) -> Result<Renewal, SignInError> {
    if read_bounded(&profile.join(STATE_FILE), STATE_CAP)?.is_none() {
        if unfinished_sign_in(profile)? {
            return Err(SignInError::Unfinished);
        }
        return Ok(Renewal::NotSignedIn);
    }
    let _owner = own(profile).await?;
    #[cfg(windows)]
    migration::recover(profile).map_err(migration_error)?;
    let Some(current) = load(profile)? else {
        return Ok(Renewal::NotSignedIn);
    };
    // Format migration never changes the generation, and completes or rolls
    // back both files before any external refresh can rotate its tokens.
    #[cfg(windows)]
    migration::migrate(profile, &current).map_err(migration_error)?;
    let now = clock.now().as_millis();
    let ahead = i64::try_from(REFRESH_AHEAD.as_millis()).unwrap_or(i64::MAX);
    if current.expires_at.as_millis().saturating_sub(now) > ahead {
        project(profile, &current)?;
        return Ok(Renewal::Current);
    }
    match refresh(client, issuer, &current, clock).await {
        Ok(renewed) => {
            save(profile, &renewed)?;
            Ok(Renewal::Refreshed)
        }
        Err(error) if current.expires_at.as_millis() > clock.now().as_millis() => {
            project(profile, &current)?;
            Ok(Renewal::Deferred(error))
        }
        Err(error) => Err(SignInError::Expired(Box::new(error))),
    }
}

/// Take the profile's sign-in lock, waiting at most [`LOCK_ALLOWANCE`] for
/// another start that is renewing the same sign-in. Released on drop.
async fn own(profile: &Path) -> Result<std::fs::File, SignInError> {
    own_files(profile, &SignInFiles::legacy()).await
}

async fn own_files(profile: &Path, files: &SignInFiles) -> Result<std::fs::File, SignInError> {
    let lock = files.open_lock(profile)?;
    let deadline = tokio::time::Instant::now() + LOCK_ALLOWANCE;
    loop {
        match lock.try_lock() {
            Ok(()) => {
                // Every owner finishes or discards an interrupted switch
                // before it reads the sign-in.
                if files.is_legacy() {
                    switch::recover(profile).map_err(|error| {
                        SignInError::Store(format!(
                            "an interrupted sign-in switch could not be finished: {:?}",
                            error.kind()
                        ))
                    })?;
                }
                return Ok(lock);
            }
            Err(std::fs::TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(LOCK_POLL).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(SignInError::Store(
                    "is being renewed by another grapt; try again in a minute".into(),
                ))
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(SignInError::Store(format!(
                    "lock could not be taken: {:?}",
                    error.kind()
                )))
            }
        }
    }
}

/// Read a regular file of at most `cap` bytes, reading no more than `cap + 1`
/// whatever its size. `None` when it does not exist.
///
/// Anything but a regular file is refused before it is opened, because
/// opening a FIFO blocks until it has a writer; the open is non-blocking as
/// well, and the opened handle must still be that same regular file.
fn read_bounded(path: &Path, cap: usize) -> Result<Option<Vec<u8>>, SignInError> {
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let failed = |error: std::io::Error| {
        SignInError::Store(format!("{name} is unreadable: {:?}", error.kind()))
    };
    let irregular = || SignInError::Store(format!("{name} is not a regular file"));
    let protected = name.ends_with(".oauth.json") || name.ends_with(".credential");
    let stored_cap = if protected {
        crate::profile_security::stored_bound(cap)
    } else {
        cap
    };
    if protected && name != STATE_FILE && name != CREDENTIAL_FILE {
        return match crate::profile_security::read_owner_only_file(path, stored_cap) {
            Ok(bytes) => crate::profile_security::open(&bytes, cap)
                .map(Some)
                .map_err(failed),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(failed(error)),
        };
    }
    let before = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failed(error)),
    };
    if !before.is_file() {
        return Err(irregular());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failed(error)),
    };
    let opened = file.metadata().map_err(failed)?;
    if !opened.is_file() || !same_file(&before, &opened) {
        return Err(irregular());
    }
    let mut bytes = Vec::new();
    file.take(
        u64::try_from(stored_cap)
            .unwrap_or(u64::MAX)
            .saturating_add(1),
    )
    .read_to_end(&mut bytes)
    .map_err(failed)?;
    if bytes.len() > stored_cap {
        return Err(SignInError::Store(format!("{name} is too large")));
    }
    if protected {
        bytes = crate::profile_security::open(&bytes, cap).map_err(failed)?;
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
fn same_file(before: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == opened.dev() && before.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(_before: &std::fs::Metadata, _opened: &std::fs::Metadata) -> bool {
    true
}

/// Write `name` in `directory` through a private temporary file and an
/// atomic rename, so a reader sees the old bytes or the new, never a mix.
fn replace(directory: &Path, name: &str, bytes: &[u8]) -> Result<(), SignInError> {
    let protected = crate::profile_security::seal(bytes)
        .map_err(|error| write_failure(name, "protected", &error))?;
    write_replacing(directory, name, protected.as_slice())
}

fn write_failure(name: &str, step: &str, error: &std::io::Error) -> SignInError {
    SignInError::Store(format!(
        "{name} could not be {step}: {}",
        crate::profile_security::failure_diagnostic(error)
    ))
}

/// Write `bytes` as `name` through a private temporary and one rename.
fn write_replacing(directory: &Path, name: &str, bytes: &[u8]) -> Result<(), SignInError> {
    crate::profile_security::replace_sign_in_file(directory, name, bytes)
        .map_err(|error| write_failure(name, "published", &error))
}

/// Make a rename in `directory` durable. Best effort: not every platform
/// can open a directory for syncing.
fn sync_directory(directory: &Path) {
    if let Ok(handle) = std::fs::File::open(directory) {
        let _ = handle.sync_all();
    }
}

#[cfg(test)]
#[path = "chatgpt/tests.rs"]
mod tests;

#[cfg(any(windows, test))]
#[path = "chatgpt/migration.rs"]
mod migration;

#[path = "chatgpt/switch.rs"]
mod switch;
pub use switch::SwitchFile;

#[cfg(windows)]
fn migration_error(error: std::io::Error) -> SignInError {
    SignInError::Store(format!(
        "protection migration refused: {} ({})",
        crate::profile_security::failure_diagnostic(&error),
        migration::failure_code(&error)
    ))
}
