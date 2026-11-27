// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bounded pure metadata/callback parsing, explicit entropy,
// always server-specific callback paths, S256 PKCE and constant-time CSRF check.
// commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use grapt_types::Secret;
use serde_json::Value;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use url::Url;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OAuthError {
    #[error("OAuth metadata or callback exceeds its bound or is malformed")]
    Format,
    #[error("OAuth endpoints must use HTTPS without URL credentials or fragments")]
    Endpoint,
    #[error("OAuth issuer does not match the selected authorization server")]
    Issuer,
    #[error("OAuth authorization server must advertise S256 PKCE")]
    Pkce,
    #[error("OAuth callback does not match its redirect URI or state")]
    Callback,
    #[error("OAuth authorization was declined")]
    Denied,
    #[error("OAuth callback has already been consumed")]
    Consumed,
}

#[derive(Clone)]
pub struct OAuthMetadata {
    issuer: String,
    authorization: Url,
    token: Url,
    registration: Option<Url>,
    issuer_response_required: bool,
}

impl std::fmt::Debug for OAuthMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthMetadata").finish_non_exhaustive()
    }
}

fn text<'a>(value: &'a Value, name: &str, max: usize) -> Result<&'a str, OAuthError> {
    value[name]
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= max)
        .ok_or(OAuthError::Format)
}

fn endpoint(raw: &str) -> Result<Url, OAuthError> {
    if raw.len() > 8192 || raw.chars().any(char::is_control) || raw.trim() != raw {
        return Err(OAuthError::Endpoint);
    }
    let url = Url::parse(raw).map_err(|_| OAuthError::Endpoint)?;
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(OAuthError::Endpoint);
    }
    Ok(url)
}

/// Security-sensitive top-level metadata fields cannot be duplicated. Unknown
/// fields remain forward-compatible, within the document's byte/field bounds.
pub(crate) fn metadata_value(bytes: &[u8]) -> Result<Value, OAuthError> {
    use serde::de::{MapAccess, Visitor};
    struct Object;
    impl<'de> Visitor<'de> for Object {
        type Value = Value;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("one metadata object with unique fields")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Value, A::Error> {
            let mut values = serde_json::Map::new();
            while let Some(key) = input.next_key::<String>()? {
                if values.len() == 128 || values.contains_key(&key) {
                    return Err(serde::de::Error::custom(
                        "metadata fields are duplicated or exceed their bound",
                    ));
                }
                values.insert(key, input.next_value::<Value>()?);
            }
            Ok(Value::Object(values))
        }
    }
    if bytes.len() > 65536 {
        return Err(OAuthError::Format);
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = serde::Deserializer::deserialize_map(&mut decoder, Object)
        .map_err(|_| OAuthError::Format)?;
    decoder.end().map_err(|_| OAuthError::Format)?;
    Ok(value)
}

impl OAuthMetadata {
    /// The caller must fetch metadata from a validated discovery URI and keep
    /// the selected issuer's original bytes; URL normalization is not issuer
    /// identity comparison (MCP 2026-07-28 / RFC 9207).
    pub fn parse(bytes: &[u8], expected_issuer: &str) -> Result<Self, OAuthError> {
        if bytes.len() > 64 * 1024 {
            return Err(OAuthError::Format);
        }
        let value = metadata_value(bytes)?;
        let issuer = text(&value, "issuer", 8192)?;
        if issuer != expected_issuer {
            return Err(OAuthError::Issuer);
        }
        let issuer_url = endpoint(issuer)?;
        if issuer_url.query().is_some() {
            return Err(OAuthError::Issuer);
        }
        let authorization = endpoint(text(&value, "authorization_endpoint", 8192)?)?;
        let token = endpoint(text(&value, "token_endpoint", 8192)?)?;
        let methods = value["code_challenge_methods_supported"]
            .as_array()
            .filter(|values| values.len() <= 16)
            .ok_or(OAuthError::Pkce)?;
        if !methods.iter().any(|method| method.as_str() == Some("S256")) {
            return Err(OAuthError::Pkce);
        }
        let issuer_response_required =
            match value.get("authorization_response_iss_parameter_supported") {
                None => false,
                Some(Value::Bool(value)) => *value,
                _ => return Err(OAuthError::Format),
            };
        if !issuer_response_required
            && authorization.origin() != issuer_url.origin()
            && authorization.origin() != token.origin()
        {
            return Err(OAuthError::Issuer);
        }
        let registration = value
            .get("registration_endpoint")
            .map(|value| endpoint(value.as_str().ok_or(OAuthError::Format)?))
            .transpose()?;
        // A metadata endpoint cannot pre-fill security-sensitive parameters
        // that the client later appends a second time.
        for url in [&authorization, &token] {
            if url.query_pairs().any(|(name, _)| {
                matches!(
                    name.as_ref(),
                    "response_type"
                        | "client_id"
                        | "redirect_uri"
                        | "state"
                        | "scope"
                        | "resource"
                        | "code_challenge"
                        | "code_challenge_method"
                        | "grant_type"
                        | "code"
                        | "code_verifier"
                        | "refresh_token"
                )
            }) {
                return Err(OAuthError::Endpoint);
            }
        }
        Ok(Self {
            issuer: issuer.to_owned(),
            authorization,
            token,
            registration,
            issuer_response_required,
        })
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn token_endpoint(&self) -> &Url {
        &self.token
    }
    pub fn registration_endpoint(&self) -> Option<&Url> {
        self.registration.as_ref()
    }
}

pub struct AuthorizationFlow {
    metadata: OAuthMetadata,
    resource: Secret<String>,
    client: String,
    redirect: Url,
    authorization: Secret<String>,
    state: Secret<String>,
    verifier: Option<Secret<String>>,
}

impl std::fmt::Debug for AuthorizationFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorizationFlow")
            .field("consumed", &self.verifier.is_none())
            .finish_non_exhaustive()
    }
}

pub struct CodeExchange {
    pub token_endpoint: Url,
    pub form: Secret<String>,
}

impl std::fmt::Debug for CodeExchange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeExchange").finish_non_exhaustive()
    }
}

pub fn callback_uri(resource: &str, callback_port: u16) -> Result<Url, OAuthError> {
    let resource = endpoint(resource)?;
    if callback_port == 0 {
        return Err(OAuthError::Format);
    }
    let callback_id = URL_SAFE_NO_PAD.encode(Sha256::digest(resource.as_str().as_bytes()));
    Url::parse(&format!(
        "http://127.0.0.1:{callback_port}/mcp/oauth/{callback_id}"
    ))
    .map_err(|_| OAuthError::Callback)
}

impl AuthorizationFlow {
    /// The production caller supplies independent, fresh 256-bit OS draws.
    /// Identity-generator tokens are not an entropy source. The loopback
    /// listener must already own its port before this flow is constructed.
    pub fn new(
        metadata: OAuthMetadata,
        resource: &str,
        client: &str,
        callback_port: u16,
        verifier_entropy: Secret<[u8; 32]>,
        state_entropy: Secret<[u8; 32]>,
        scope: Option<&str>,
    ) -> Result<Self, OAuthError> {
        let resource_url = endpoint(resource)?;
        if client.is_empty()
            || client.len() > 8192
            || client.chars().any(char::is_control)
            || callback_port == 0
        {
            return Err(OAuthError::Format);
        }
        if scope.is_some_and(|scope| {
            scope.len() > 4096
                || !scope.bytes().all(|byte| {
                    byte == b' ' || (0x21..=0x7e).contains(&byte) && !matches!(byte, b'"' | b'\\')
                })
        }) {
            return Err(OAuthError::Format);
        }
        let redirect = callback_uri(resource_url.as_str(), callback_port)?;
        let verifier = URL_SAFE_NO_PAD.encode(verifier_entropy.expose());
        let state = URL_SAFE_NO_PAD.encode(state_entropy.expose());
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut authorization = metadata.authorization.clone();
        {
            let mut query = authorization.query_pairs_mut();
            query
                .append_pair("response_type", "code")
                .append_pair("client_id", client)
                .append_pair("redirect_uri", redirect.as_str())
                .append_pair("state", &state)
                .append_pair("resource", resource_url.as_str())
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256");
            if let Some(scope) = scope {
                query.append_pair("scope", scope);
            }
        }
        if authorization.as_str().len() > 32 * 1024 {
            return Err(OAuthError::Format);
        }
        Ok(Self {
            metadata,
            resource: Secret::new(resource_url.to_string()),
            client: client.to_owned(),
            redirect,
            authorization: Secret::new(authorization.to_string()),
            state: Secret::new(state),
            verifier: Some(Secret::new(verifier)),
        })
    }

    pub fn authorization_url(&self) -> &Secret<String> {
        &self.authorization
    }
    pub fn redirect_uri(&self) -> &Url {
        &self.redirect
    }

    /// Invalid attempts do not consume the flow. Its bounded listener limits
    /// attempts and total lifetime; a valid success/error consumes it once.
    pub fn accept_callback(&mut self, raw: &str) -> Result<CodeExchange, OAuthError> {
        if self.verifier.is_none() {
            return Err(OAuthError::Consumed);
        }
        if raw.len() > 16 * 1024 || raw.chars().any(char::is_control) {
            return Err(OAuthError::Format);
        }
        if raw.split('?').next() != Some(self.redirect.as_str()) {
            return Err(OAuthError::Callback);
        }
        let callback = Url::parse(raw).map_err(|_| OAuthError::Callback)?;
        if callback.origin() != self.redirect.origin()
            || callback.path() != self.redirect.path()
            || callback.fragment().is_some()
            || !callback.username().is_empty()
            || callback.password().is_some()
        {
            return Err(OAuthError::Callback);
        }
        let mut parameters = std::collections::BTreeMap::new();
        for pair in callback
            .query()
            .unwrap_or_default()
            .split('&')
            .filter(|part| !part.is_empty())
        {
            let (name, value) = pair.split_once('=').ok_or(OAuthError::Format)?;
            let name = form_component(name, 128)?;
            let value = form_component(value, 8192)?;
            if parameters.len() == 16 || name.is_empty() || parameters.insert(name, value).is_some()
            {
                return Err(OAuthError::Format);
            }
        }
        let state = parameters.get("state").ok_or(OAuthError::Callback)?;
        if !bool::from(self.state.expose().as_bytes().ct_eq(state.as_bytes())) {
            return Err(OAuthError::Callback);
        }
        // Compare the form-decoded issuer byte-for-byte, including on errors.
        // Never normalize a present issuer or display an unvalidated error.
        match parameters.get("iss") {
            Some(issuer) if issuer != &self.metadata.issuer => return Err(OAuthError::Issuer),
            None if self.metadata.issuer_response_required => return Err(OAuthError::Issuer),
            _ => {}
        }
        let code = parameters.get("code");
        if parameters.contains_key("error") {
            if code.is_some() {
                return Err(OAuthError::Format);
            }
            drop(self.verifier.take());
            return Err(OAuthError::Denied);
        }
        let code = code
            .filter(|code| {
                !code.is_empty() && code.len() <= 8192 && !code.chars().any(char::is_control)
            })
            .ok_or(OAuthError::Format)?;
        let verifier = self.verifier.take().ok_or(OAuthError::Consumed)?;
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "authorization_code")
            .append_pair("client_id", &self.client)
            .append_pair("code", code)
            .append_pair("code_verifier", verifier.expose())
            .append_pair("redirect_uri", self.redirect.as_str())
            .append_pair("resource", self.resource.expose())
            .finish();
        Ok(CodeExchange {
            token_endpoint: self.metadata.token.clone(),
            form: Secret::new(form),
        })
    }
}

// URL's general form iterator replaces invalid UTF-8. A security callback must
// instead reject malformed percent escapes and preserve the decoded issuer.
fn form_component(raw: &str, maximum: usize) -> Result<String, OAuthError> {
    let mut decoded = Vec::with_capacity(raw.len().min(maximum));
    let mut bytes = raw.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        let byte = match byte {
            b'+' => b' ',
            b'%' => {
                let high = bytes.next().and_then(hex).ok_or(OAuthError::Format)?;
                let low = bytes.next().and_then(hex).ok_or(OAuthError::Format)?;
                (high << 4) | low
            }
            byte => byte,
        };
        if decoded.len() == maximum || byte.is_ascii_control() {
            return Err(OAuthError::Format);
        }
        decoded.push(byte);
    }
    String::from_utf8(decoded).map_err(|_| OAuthError::Format)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[path = "oauth/tests.rs"]
mod tests;

pub mod callback;
pub mod challenge;
pub mod credential_lock;
pub mod discovery;
pub mod http;
pub mod registration;
pub(crate) mod ssrf;
pub mod tokens;
