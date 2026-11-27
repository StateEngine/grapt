//! Service-account JWT exchange for native Vertex Gemini connections.
//! Adapted from yup-oauth2's service_account.rs; bounded, fixed-issuer and clock-injected.
//! Copyright (c) 2016 Google Inc (lewinb@google.com).
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use grapt_types::{ClockAuthority, Timestamp};
use rustls::{
    pki_types::{pem::PemObject, PrivateKeyDer},
    sign::SigningKey,
};
use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

pub mod adc;

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

#[derive(Debug, thiserror::Error)]
pub enum VertexError {
    #[error("enter a valid Google service-account JSON key")]
    Invalid,
    #[error("Google service-account authorization failed; check the key and system clock")]
    Authorization,
    #[error("Google authorization is unavailable; retry the connection")]
    Unavailable,
    #[error("Google access token expired; reconnect the Vertex account")]
    Expired,
}

/// App-entered key is retained only in zeroizing storage, never a file reference.
#[allow(
    missing_debug_implementations,
    reason = "service-account material is secret"
)]
pub struct ServiceAccount(pub(crate) Zeroizing<String>);
impl ServiceAccount {
    pub fn new(raw: String) -> Result<Self, VertexError> {
        let raw = Zeroizing::new(raw);
        validate_key(&raw)?;
        Ok(Self(raw))
    }
}

#[derive(Deserialize)]
struct Key {
    #[serde(rename = "type")]
    kind: String,
    private_key: String,
    client_email: String,
    token_uri: String,
    private_key_id: Option<String>,
    universe_domain: Option<String>,
}
impl Drop for Key {
    fn drop(&mut self) {
        self.private_key.zeroize();
    }
}
fn key(raw: &str) -> Result<Key, VertexError> {
    if raw.is_empty() || raw.len() > 16 * 1024 {
        return Err(VertexError::Invalid);
    }
    let key: Key = serde_json::from_str(raw).map_err(|_| VertexError::Invalid)?;
    if key.kind != "service_account"
        || key.token_uri != TOKEN_URL
        || key
            .universe_domain
            .as_deref()
            .is_some_and(|domain| domain != "googleapis.com")
        || key.private_key.len() > 8192
        || key.client_email.len() > 256
        || (!key.client_email.ends_with(".gserviceaccount.com")
            || key.client_email.matches('@').count() != 1
            || key.client_email.starts_with('@'))
        || !key
            .client_email
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"@.-_".contains(&byte))
        || key.private_key_id.as_ref().is_some_and(|id| {
            id.is_empty()
                || id.len() > 256
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        })
    {
        return Err(VertexError::Invalid);
    }
    Ok(key)
}
fn signer(key: &Key) -> Result<Box<dyn rustls::sign::Signer>, VertexError> {
    let der = PrivateKeyDer::from_pem_slice(key.private_key.as_bytes())
        .map_err(|_| VertexError::Invalid)?;
    rustls::crypto::ring::sign::RsaSigningKey::new(&der)
        .map_err(|_| VertexError::Invalid)?
        .choose_scheme(&[rustls::SignatureScheme::RSA_PKCS1_SHA256])
        .ok_or(VertexError::Invalid)
}
/// Validates the bounded credential without contacting its declared URLs.
pub fn validate_key(raw: &str) -> Result<(), VertexError> {
    signer(&key(raw)?).map(|_| ())
}

/// Access material has no formatter or codec and zeroizes on drop.
#[allow(missing_debug_implementations, reason = "access material is secret")]
pub struct Token {
    access: Zeroizing<String>,
    expires_at: Timestamp,
}
impl Token {
    pub fn from_cached(
        access: Zeroizing<String>,
        expires_at: Timestamp,
    ) -> Result<Self, VertexError> {
        if access.is_empty()
            || access.len() > 4096
            || !access.bytes().all(|b| b.is_ascii_graphic())
            || expires_at.as_millis() <= 0
        {
            return Err(VertexError::Invalid);
        }
        Ok(Self { access, expires_at })
    }
    pub fn access(&self) -> &str {
        &self.access
    }
    pub fn expires_at(&self) -> Timestamp {
        self.expires_at
    }
}

/// Fixed Google issuer, no redirects, proxies or caller-selected token endpoint.
pub async fn exchange(
    raw: &str,
    clock: &(dyn ClockAuthority + Sync),
) -> Result<Token, VertexError> {
    exchange_at(raw, clock, &token_endpoint()?).await
}
fn token_endpoint() -> Result<String, VertexError> {
    #[cfg(feature = "loopback-testkit")]
    match std::env::var("GRAPT_VERTEX_TEST_TOKEN_ENDPOINT") {
        Ok(raw) => {
            if raw.len() > 2048 {
                return Err(VertexError::Invalid);
            }
            let url = reqwest::Url::parse(&raw).map_err(|_| VertexError::Invalid)?;
            if url.scheme() != "http"
                || url.host_str() != Some("127.0.0.1")
                || url.port().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/token"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(VertexError::Invalid);
            }
            return Ok(url.into());
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(_) => return Err(VertexError::Invalid),
    }
    Ok(TOKEN_URL.to_owned())
}

async fn exchange_at(
    raw: &str,
    clock: &(dyn ClockAuthority + Sync),
    endpoint: &str,
) -> Result<Token, VertexError> {
    let key = key(raw)?;
    let now = clock.now().as_millis();
    if now < 0 {
        return Err(VertexError::Invalid);
    }
    let iat = now / 1000;
    let exp = iat.checked_add(3595).ok_or(VertexError::Invalid)?;
    #[derive(serde::Serialize)]
    struct Header<'a> {
        alg: &'static str,
        typ: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        kid: Option<&'a str>,
    }
    let header = Header {
        alg: "RS256",
        typ: "JWT",
        kid: key.private_key_id.as_deref(),
    };
    let claims = serde_json::json!({"iss":key.client_email,"aud":TOKEN_URL,"scope":SCOPE,"iat":iat,"exp":exp});
    let mut assertion = Zeroizing::new(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).map_err(|_| VertexError::Invalid)?),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).map_err(|_| VertexError::Invalid)?)
    ));
    let signature = signer(&key)?
        .sign(assertion.as_bytes())
        .map_err(|_| VertexError::Invalid)?;
    assertion.push('.');
    URL_SAFE_NO_PAD.encode_string(signature, &mut assertion);
    let body = Zeroizing::new(format!(
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={}",
        assertion.as_str()
    ));
    let client = crate::chatgpt::bounded_client(endpoint.starts_with("https://"))
        .map_err(|_| VertexError::Unavailable)?;
    let mut response = client
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.as_bytes().to_vec())
        .send()
        .await
        .map_err(|_| VertexError::Unavailable)?;
    if !response.status().is_success() {
        return Err(VertexError::Authorization);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| VertexError::Unavailable)?
    {
        if chunk.len() > (16 * 1024_usize).saturating_sub(bytes.len()) {
            return Err(VertexError::Authorization);
        }
        bytes.extend_from_slice(&chunk);
    }
    #[derive(Deserialize)]
    struct Reply {
        access_token: String,
        token_type: String,
        expires_in: i64,
    }
    impl Drop for Reply {
        fn drop(&mut self) {
            self.access_token.zeroize();
        }
    }
    let mut reply: Reply =
        serde_json::from_slice(&bytes).map_err(|_| VertexError::Authorization)?;
    if !reply.token_type.eq_ignore_ascii_case("Bearer") || !(1..=3600).contains(&reply.expires_in) {
        return Err(VertexError::Authorization);
    }
    let expires = now
        .checked_add(reply.expires_in * 1000)
        .ok_or(VertexError::Authorization)?;
    if expires <= clock.now().as_millis() {
        return Err(VertexError::Expired);
    }
    Token::from_cached(
        Zeroizing::new(std::mem::take(&mut reply.access_token)),
        Timestamp::from_millis(expires),
    )
}

/// Refreshes are memory-only; the original key stays in the existing vault.
#[allow(
    missing_debug_implementations,
    reason = "session owns credential material"
)]
pub struct Session {
    raw: Zeroizing<String>,
    token: Token,
}
impl Session {
    pub fn new(raw: Zeroizing<String>, token: Token) -> Result<Self, VertexError> {
        validate_key(&raw)?;
        Ok(Self { raw, token })
    }
    pub fn matches_key(&self, raw: &str) -> bool {
        self.raw.as_str() == raw
    }
    pub fn token(&self) -> &Token {
        &self.token
    }
    pub async fn prepare(
        &mut self,
        clock: &(dyn ClockAuthority + Sync),
        allow_refresh: bool,
        force: bool,
    ) -> Result<bool, VertexError> {
        let now = clock.now().as_millis();
        if !force && self.token.expires_at.as_millis().saturating_sub(now) > 60_000 {
            return Ok(false);
        }
        if !allow_refresh {
            return if self.token.expires_at.as_millis() > now {
                Ok(false)
            } else {
                Err(VertexError::Expired)
            };
        }
        self.token = exchange(&self.raw, clock).await?;
        Ok(true)
    }
}
