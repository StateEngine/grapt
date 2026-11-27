//! Fixed IBM IAM API-key exchange and memory-only access-token renewal.
//! Adapted from IBM/watsonx-ai-provider watsonx-iam.ts at
//! f0021a6511cad5c15a73ddbe351f3e3495f0f8bc; see licenses/ibm-watsonx/.
//! Copyright (c) 2025 International Business Machines.
use grapt_types::{ClockAuthority, Timestamp};
use serde::Deserialize;
use zeroize::{Zeroize, Zeroizing};

const ISSUER: &str = "https://iam.cloud.ibm.com/identity/token";

#[derive(Debug, thiserror::Error)]
pub enum WatsonxError {
    #[error("enter a valid IBM Cloud API key")]
    Invalid,
    #[error("IBM IAM refused authorization; check the key and project permissions")]
    Authorization,
    #[error("IBM IAM is unavailable; retry Connect")]
    Unavailable,
    #[error("IBM access expired; reconnect watsonx")]
    Expired,
}

#[allow(missing_debug_implementations, reason = "the API key is secret")]
pub struct ApiKey(pub(crate) Zeroizing<String>);
impl ApiKey {
    pub fn new(value: String) -> Result<Self, WatsonxError> {
        let value = Zeroizing::new(value);
        if !valid(&value) {
            return Err(WatsonxError::Invalid);
        }
        Ok(Self(value))
    }
}

pub(crate) fn valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= 4096 && value.bytes().all(|b| b.is_ascii_graphic())
}

#[allow(missing_debug_implementations, reason = "access material is secret")]
pub struct Token {
    access: Zeroizing<String>,
    expires_at: Timestamp,
}
impl Token {
    pub fn from_cached(
        access: Zeroizing<String>,
        expires_at: Timestamp,
    ) -> Result<Self, WatsonxError> {
        if !valid(&access) || expires_at.as_millis() <= 0 {
            return Err(WatsonxError::Invalid);
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

fn token_endpoint() -> Result<String, WatsonxError> {
    #[cfg(feature = "loopback-testkit")]
    match std::env::var("GRAPT_WATSONX_TEST_TOKEN_ENDPOINT") {
        Ok(value) => {
            if value.len() > 2048 {
                return Err(WatsonxError::Invalid);
            }
            let url = reqwest::Url::parse(&value).map_err(|_| WatsonxError::Invalid)?;
            if url.scheme() != "http"
                || url.host_str() != Some("127.0.0.1")
                || url.port().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/identity/token"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(WatsonxError::Invalid);
            }
            return Ok(value);
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(_) => return Err(WatsonxError::Invalid),
    }
    Ok(ISSUER.into())
}

pub async fn exchange(
    key: &str,
    clock: &(dyn ClockAuthority + Sync),
) -> Result<Token, WatsonxError> {
    if !valid(key) {
        return Err(WatsonxError::Invalid);
    }
    let now = clock.now().as_millis();
    if now <= 0 {
        return Err(WatsonxError::Invalid);
    }
    let body = Zeroizing::new(crate::chatgpt::form(&[
        ("grant_type", "urn:ibm:params:oauth:grant-type:apikey"),
        ("apikey", key),
    ]));
    let endpoint = token_endpoint()?;
    let client = crate::chatgpt::bounded_client(endpoint.starts_with("https://"))
        .map_err(|_| WatsonxError::Unavailable)?;
    let mut response = client
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("accept", "application/json")
        .body(body.as_bytes().to_vec())
        .send()
        .await
        .map_err(|_| WatsonxError::Unavailable)?;
    if matches!(response.status().as_u16(), 401 | 403) {
        return Err(WatsonxError::Authorization);
    }
    if !response.status().is_success() {
        return Err(WatsonxError::Unavailable);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| WatsonxError::Unavailable)?
    {
        if chunk.len() > (16 * 1024usize).saturating_sub(bytes.len()) {
            return Err(WatsonxError::Authorization);
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
        serde_json::from_slice(&bytes).map_err(|_| WatsonxError::Authorization)?;
    if !reply.token_type.eq_ignore_ascii_case("Bearer") || !(1..=3600).contains(&reply.expires_in) {
        return Err(WatsonxError::Authorization);
    }
    let expires = now
        .checked_add(reply.expires_in * 1000)
        .ok_or(WatsonxError::Authorization)?;
    if expires <= clock.now().as_millis() {
        return Err(WatsonxError::Expired);
    }
    Token::from_cached(
        std::mem::take(&mut reply.access_token).into(),
        Timestamp::from_millis(expires),
    )
}

#[allow(
    missing_debug_implementations,
    reason = "renewal owns the original API key"
)]
pub struct Session {
    key: ApiKey,
    token: Token,
}
impl Session {
    pub fn new(key: String, token: Token) -> Result<Self, WatsonxError> {
        Ok(Self {
            key: ApiKey::new(key)?,
            token,
        })
    }
    pub fn token(&self) -> &Token {
        &self.token
    }
    pub fn matches_key(&self, candidate: &str) -> bool {
        self.key.0.as_str() == candidate
    }
    pub async fn prepare(
        &mut self,
        clock: &(dyn ClockAuthority + Sync),
        allow: bool,
        force: bool,
    ) -> Result<bool, WatsonxError> {
        let now = clock.now().as_millis();
        if now <= 0 {
            return Err(WatsonxError::Invalid);
        }
        if !force && self.token.expires_at.as_millis().saturating_sub(now) > 300_000 {
            return Ok(false);
        }
        if !allow {
            return if self.token.expires_at.as_millis() > now {
                Ok(false)
            } else {
                Err(WatsonxError::Expired)
            };
        }
        self.token = exchange(&self.key.0, clock).await?;
        Ok(true)
    }
}
