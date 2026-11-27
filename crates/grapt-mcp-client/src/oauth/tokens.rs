// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: explicit injected timestamps, bounded Secret custody,
// exact issuer/resource/client binding, store-owned persistence, no keyring/file
// fallback or publicly comparable secret fingerprint.
// commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d.

use crate::oauth::OAuthMetadata;
use grapt_types::{McpLoginId, Secret};
use serde::Deserialize;

const MAX_BYTES: usize = 65536;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    #[error("OAuth token response or stored credential is malformed or exceeds its bound")]
    Format,
    #[error("OAuth credential issuer, resource, or client binding changed; sign in again")]
    Binding,
    #[error("OAuth credential has no refresh token; sign in again")]
    LoginRequired,
}

struct SecretText(Secret<String>);
impl<'de> Deserialize<'de> for SecretText {
    fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        String::deserialize(decoder).map(|text| Self(Secret::new(text)))
    }
}

#[derive(Deserialize)]
struct Response {
    access_token: SecretText,
    token_type: String,
    refresh_token: Option<SecretText>,
    expires_in: Option<u64>,
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    generation: String,
    issuer: String,
    resource: SecretText,
    client: SecretText,
    access: SecretText,
    refresh: Option<SecretText>,
    expires_ms: Option<u64>,
}

pub struct OAuthCredential {
    generation: McpLoginId,
    issuer: String,
    resource: Secret<String>,
    client: Secret<String>,
    access: Secret<String>,
    refresh: Option<Secret<String>>,
    expires_ms: Option<u64>,
}

impl std::fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthCredential").finish_non_exhaustive()
    }
}

fn valid_secret(value: &Secret<String>, header: bool) -> bool {
    let value = value.expose();
    !value.is_empty()
        && value.len() <= 16384
        && value.bytes().all(|byte| {
            if header {
                byte.is_ascii_alphanumeric() || b"-._~+/=".contains(&byte)
            } else {
                (0x20..=0x7e).contains(&byte)
            }
        })
}

#[derive(Debug)]
pub enum TokenGrant<'a> {
    Login(McpLoginId),
    Refresh(&'a OAuthCredential),
}

impl OAuthCredential {
    pub fn from_response(
        metadata: &OAuthMetadata,
        resource: &str,
        client: &str,
        response: &Secret<Vec<u8>>,
        received_ms: u64,
        grant: TokenGrant<'_>,
    ) -> Result<Self, TokenError> {
        if response.expose().len() > MAX_BYTES {
            return Err(TokenError::Format);
        }
        let (generation, previous) = match grant {
            TokenGrant::Login(generation) => (generation, None),
            TokenGrant::Refresh(previous) => {
                previous.check_binding(metadata, resource, client)?;
                (previous.generation.clone(), Some(previous))
            }
        };
        let response: Response =
            serde_json::from_slice(response.expose()).map_err(|_| TokenError::Format)?;
        if response.error.is_some() || !response.token_type.eq_ignore_ascii_case("Bearer") {
            return Err(TokenError::Format);
        }
        let expires_ms = response
            .expires_in
            .map(|seconds| {
                seconds
                    .checked_mul(1000)
                    .and_then(|ms| received_ms.checked_add(ms))
                    .ok_or(TokenError::Format)
            })
            .transpose()?;
        let refresh = response.refresh_token.map(|token| token.0).or_else(|| {
            previous
                .and_then(|previous| previous.refresh.as_ref())
                .map(|token| Secret::new(token.expose().clone()))
        });
        let credential = Self {
            generation,
            issuer: metadata.issuer().to_owned(),
            resource: Secret::new(resource.to_owned()),
            client: Secret::new(client.to_owned()),
            access: response.access_token.0,
            refresh,
            expires_ms,
        };
        credential.validate()?;
        Ok(credential)
    }

    fn validate(&self) -> Result<(), TokenError> {
        let valid_url = |raw: &str| {
            raw.len() <= 8192
                && !raw.chars().any(char::is_control)
                && url::Url::parse(raw).is_ok_and(|url| {
                    url.scheme() == "https"
                        && url.host().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.fragment().is_none()
                })
        };
        if self.generation.as_str().len() > 128
            || !valid_url(&self.issuer)
            || !valid_url(self.resource.expose())
            || url::Url::parse(&self.issuer).is_ok_and(|url| url.query().is_some())
            || self.client.expose().is_empty()
            || self.client.expose().len() > 8192
            || self.client.expose().chars().any(char::is_control)
            || !valid_secret(&self.access, true)
            || self
                .refresh
                .as_ref()
                .is_some_and(|refresh| !valid_secret(refresh, false))
        {
            return Err(TokenError::Format);
        }
        Ok(())
    }

    pub fn generation(&self) -> &McpLoginId {
        &self.generation
    }
    pub fn client_id(&self) -> &Secret<String> {
        &self.client
    }
    pub fn check_request(
        &self,
        resource: &str,
        configured_client: Option<&str>,
    ) -> Result<(), TokenError> {
        if self.resource.expose() != resource
            || configured_client.is_some_and(|client| client != self.client.expose())
        {
            return Err(TokenError::Binding);
        }
        Ok(())
    }

    pub fn check_binding(
        &self,
        metadata: &OAuthMetadata,
        resource: &str,
        client: &str,
    ) -> Result<(), TokenError> {
        if self.issuer != metadata.issuer()
            || self.resource.expose() != resource
            || self.client.expose() != client
        {
            return Err(TokenError::Binding);
        }
        Ok(())
    }

    pub fn access_token(&self) -> &Secret<String> {
        &self.access
    }

    /// Unknown expiry is not an invented deadline. Such a credential may be
    /// used until a truthful rejection; known expiry uses an injected clock.
    pub fn needs_refresh(&self, now_ms: u64) -> bool {
        self.expires_ms
            .is_some_and(|expiry| expiry <= now_ms.saturating_add(30_000))
    }

    pub fn refresh_form(
        &self,
        metadata: &OAuthMetadata,
        resource: &str,
        client: &str,
    ) -> Result<Secret<String>, TokenError> {
        self.check_binding(metadata, resource, client)?;
        let refresh = self.refresh.as_ref().ok_or(TokenError::LoginRequired)?;
        Ok(Secret::new(
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", "refresh_token")
                .append_pair("client_id", self.client.expose())
                .append_pair("refresh_token", refresh.expose())
                .append_pair("resource", self.resource.expose())
                .finish(),
        ))
    }

    /// Only the credential store consumes this explicit sealed serialization.
    pub fn encode(&self) -> Result<Secret<String>, TokenError> {
        let value = serde_json::json!({"version":2,"generation":self.generation.as_str(),"issuer":self.issuer,
            "resource":self.resource.expose(), "client":self.client.expose(), "access":self.access.expose(),
            "refresh":self.refresh.as_ref().map(Secret::expose), "expires_ms":self.expires_ms}).to_string();
        if value.len() > MAX_BYTES {
            return Err(TokenError::Format);
        }
        Ok(Secret::new(value))
    }

    pub fn decode(value: &Secret<String>) -> Result<Self, TokenError> {
        if value.expose().len() > MAX_BYTES {
            return Err(TokenError::Format);
        }
        let stored: Stored =
            serde_json::from_str(value.expose()).map_err(|_| TokenError::Format)?;
        if stored.version != 2 {
            return Err(TokenError::Format);
        }
        let credential = Self {
            generation: stored.generation.parse().map_err(|_| TokenError::Format)?,
            issuer: stored.issuer,
            resource: stored.resource.0,
            client: stored.client.0,
            access: stored.access.0,
            refresh: stored.refresh.map(|token| token.0),
            expires_ms: stored.expires_ms,
        };
        credential.validate()?;
        Ok(credential)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn metadata(issuer: &str) -> OAuthMetadata {
        OAuthMetadata::parse(serde_json::json!({"issuer":issuer,"authorization_endpoint":"https://auth.example/authorize",
            "token_endpoint":"https://auth.example/token", "code_challenge_methods_supported":["S256"],
            "authorization_response_iss_parameter_supported":true}).to_string().as_bytes(), issuer)
            .unwrap_or_else(|error| panic!("fixture metadata: {error}"))
    }
    fn response(value: serde_json::Value) -> Secret<Vec<u8>> {
        Secret::new(value.to_string().into_bytes())
    }
    #[test]
    fn refresh_retains_omitted_rotation_and_persists_exact_expiry_without_clock_reads(
    ) -> Result<(), TokenError> {
        let metadata = metadata("https://auth.example");
        let first = OAuthCredential::from_response(
            &metadata,
            "https://mcp.example/mcp",
            "client",
            &response(
                serde_json::json!({"token_type":"Bearer", "access_token":"first", "refresh_token":"rotating", "expires_in":120}),
            ),
            1000,
            TokenGrant::Login("mcl_fixture".parse().map_err(|_| TokenError::Format)?),
        )?;
        assert!(!first.needs_refresh(90_999));
        assert!(first.needs_refresh(91_000));
        let next = OAuthCredential::from_response(
            &metadata,
            "https://mcp.example/mcp",
            "client",
            &response(
                serde_json::json!({"token_type":"bearer", "access_token":"second", "expires_in":120}),
            ),
            5000,
            TokenGrant::Refresh(&first),
        )?;
        assert_eq!(next.access_token().expose(), "second");
        assert_eq!(next.generation(), first.generation());
        let restored = OAuthCredential::decode(&next.encode()?)?;
        assert!(restored
            .refresh_form(&metadata, "https://mcp.example/mcp", "client")?
            .expose()
            .contains("refresh_token=rotating"));
        assert!(!format!("{restored:?}").contains("rotating"));
        assert!(!restored.needs_refresh(94_999));
        assert!(restored.needs_refresh(95_000));
        Ok(())
    }
    #[test]
    fn changed_binding_never_releases_a_refresh_token() -> Result<(), TokenError> {
        let metadata = metadata("https://auth.example");
        let credential = OAuthCredential::from_response(
            &metadata,
            "https://mcp.example/mcp",
            "client",
            &response(
                serde_json::json!({"token_type":"Bearer","access_token":"held", "refresh_token":"secret"}),
            ),
            1,
            TokenGrant::Login("mcl_fixture".parse().map_err(|_| TokenError::Format)?),
        )?;
        assert!(!credential.needs_refresh(u64::MAX));
        assert_eq!(
            credential
                .refresh_form(
                    &self::metadata("https://auth.example/"),
                    "https://mcp.example/mcp",
                    "client"
                )
                .err(),
            Some(TokenError::Binding)
        );
        assert_eq!(
            credential
                .refresh_form(&metadata, "https://mcp.example/other", "client")
                .err(),
            Some(TokenError::Binding)
        );
        assert_eq!(
            credential
                .refresh_form(&metadata, "https://mcp.example/mcp", "other")
                .err(),
            Some(TokenError::Binding)
        );
        Ok(())
    }
    #[test]
    fn bad_token_shapes_duplicate_fields_and_expiry_overflow_fail_honestly(
    ) -> Result<(), TokenError> {
        let metadata = metadata("https://auth.example");
        for bytes in [
            br#"{"token_type":"Bearer","access_token":"one","access_token":"two"}"#.as_slice(),
            br#"{"token_type":"Basic","access_token":"one"}"#,
            br#"{"token_type":"Bearer","access_token":"header\nleak"}"#,
            br#"{"token_type":"Bearer","access_token":"one","expires_in":18446744073709551615}"#,
            br#"{"token_type":"Bearer","access_token":"one","error":"rejected"}"#,
        ] {
            assert!(OAuthCredential::from_response(
                &metadata,
                "https://mcp.example/mcp",
                "client",
                &Secret::new(bytes.to_vec()),
                1,
                TokenGrant::Login("mcl_fixture".parse().map_err(|_| TokenError::Format)?)
            )
            .is_err());
        }
        Ok(())
    }
}
