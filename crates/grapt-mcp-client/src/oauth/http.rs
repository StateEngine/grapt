// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: direct bounded metadata/token HTTP, no ambient headers,
// redirects, legacy discovery fallback, secret response errors or POST retries.
// commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d.

use super::{
    challenge::BearerChallenge,
    discovery::{self, ProtectedResource},
    registration::PublicClient,
    ssrf, OAuthMetadata,
};
use grapt_types::Secret;
use reqwest::{header, StatusCode, Url};
use std::{sync::Arc, time::Duration};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OAuthHttpError {
    #[error("OAuth endpoint must use HTTPS without credentials or fragments")]
    Endpoint,
    #[error("OAuth HTTP client could not initialize")]
    Client,
    #[error("OAuth HTTP transport failed; a token request outcome may be unknown")]
    Transport,
    #[error("OAuth operation exceeded its deadline")]
    TimedOut,
    #[error("OAuth HTTP status {0}")]
    Status(u16),
    #[error("OAuth headers or JSON document exceed their bounds or are malformed")]
    Document,
    #[error("OAuth metadata was not found at the advertised or well-known locations")]
    MetadataMissing,
    #[error("OAuth token was rejected; sign in again")]
    TokenRejected,
    #[error("OAuth authorization server requires a pre-registered public client ID")]
    RegistrationRequired,
}

pub struct Discovered {
    pub resource: ProtectedResource,
    pub authorization: OAuthMetadata,
    pub scope: Option<String>,
}
impl std::fmt::Debug for Discovered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Discovered([REDACTED])")
    }
}
#[cfg(test)]
type DnsLookupFn = Arc<dyn Fn(&str, u16) -> Result<Vec<std::net::SocketAddr>, ()> + Send + Sync>;

#[derive(Clone)]
pub struct MetadataHttp {
    tls: Arc<rustls::ClientConfig>,
    allow_loopback: bool,
    #[cfg(test)]
    dns_lookup: Option<DnsLookupFn>,
}
impl std::fmt::Debug for MetadataHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MetadataHttp")
    }
}
impl MetadataHttp {
    pub fn new() -> Result<Self, OAuthHttpError> {
        use rustls_platform_verifier::BuilderVerifierExt;
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| OAuthHttpError::Client)?
        .with_platform_verifier()
        .map_err(|_| OAuthHttpError::Client)?
        .with_no_client_auth();
        Self::with_tls(tls, false)
    }

    fn with_tls(tls: rustls::ClientConfig, allow_loopback: bool) -> Result<Self, OAuthHttpError> {
        // Validate TLS configuration upfront by constructing a probe client.
        let tls = Arc::new(tls);
        let _ = reqwest::Client::builder()
            .use_preconfigured_tls((*tls).clone())
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .pool_max_idle_per_host(0)
            .http2_max_header_list_size(16384)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| OAuthHttpError::Client)?;
        Ok(Self {
            tls,
            allow_loopback,
            #[cfg(test)]
            dns_lookup: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_dns_lookup(mut self, lookup: DnsLookupFn) -> Self {
        self.dns_lookup = Some(lookup);
        self
    }

    #[cfg(any(test, feature = "loopback-testkit"))]
    fn with_tls_loopback(tls: rustls::ClientConfig) -> Result<Self, OAuthHttpError> {
        Self::with_tls(tls, true)
    }

    async fn pinned_client(&self, url: &Url) -> Result<reqwest::Client, OAuthHttpError> {
        #[cfg(test)]
        let resolved = if let Some(lookup) = &self.dns_lookup {
            let lookup = Arc::clone(lookup);
            ssrf::resolve_pinned_with(url, self.allow_loopback, move |host, port| {
                lookup(host, port)
            })
            .await
        } else {
            ssrf::resolve_pinned(url, self.allow_loopback).await
        };
        #[cfg(not(test))]
        let resolved = ssrf::resolve_pinned(url, self.allow_loopback).await;
        let (host, addresses) = resolved.map_err(|()| OAuthHttpError::Endpoint)?;
        reqwest::Client::builder()
            .use_preconfigured_tls((*self.tls).clone())
            .dns_resolver(Arc::new(ssrf::PinnedDns::new(host, addresses)))
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .pool_max_idle_per_host(0)
            .http2_max_header_list_size(16384)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| OAuthHttpError::Client)
    }

    pub async fn discover(
        &self,
        resource: &str,
        budget: Duration,
    ) -> Result<Discovered, OAuthHttpError> {
        timed(budget, async {
            let endpoint = https(resource, self.allow_loopback)?;
            let client = self.pinned_client(&endpoint).await?;
            // A GET probe has no configured resource headers or credentials.
            // Its body is never consumed: it may be an open MCP SSE stream.
            let response = client
                .get(endpoint)
                .header(header::ACCEPT, "application/json, text/event-stream")
                .header(header::ACCEPT_ENCODING, "identity")
                .send()
                .await
                .map_err(|_| OAuthHttpError::Transport)?;
            headers(response.headers())?;
            let fields = response
                .headers()
                .get_all(header::WWW_AUTHENTICATE)
                .iter()
                .map(|value| value.to_str().map_err(|_| OAuthHttpError::Document))
                .collect::<Result<Vec<_>, _>>()?;
            let challenge = if matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) {
                BearerChallenge::parse(&fields).map_err(|_| OAuthHttpError::Document)?
            } else {
                if !(response.status().is_success()
                    || matches!(
                        response.status(),
                        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
                    ))
                {
                    return Err(OAuthHttpError::Status(response.status().as_u16()));
                }
                None
            };
            drop(response);
            let candidates = discovery::protected_resource_candidates(
                resource,
                challenge.as_ref().and_then(BearerChallenge::metadata_url),
            )
            .map_err(|_| OAuthHttpError::Document)?;
            let mut protected = None;
            for candidate in candidates {
                if let Some(bytes) = self.metadata(candidate).await? {
                    protected = Some(
                        ProtectedResource::parse(bytes.expose(), resource)
                            .map_err(|_| OAuthHttpError::Document)?,
                    );
                    break;
                }
            }
            let protected = protected.ok_or(OAuthHttpError::MetadataMissing)?;
            for issuer in protected.issuers() {
                let candidates = discovery::authorization_server_candidates(issuer)
                    .map_err(|_| OAuthHttpError::Document)?;
                let mut unavailable = None;
                for candidate in candidates {
                    let bytes = match self.metadata(candidate).await {
                        Ok(Some(bytes)) => bytes,
                        Ok(None) => continue,
                        Err(error @ OAuthHttpError::Status(503)) => {
                            unavailable = Some(error);
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    let authorization = OAuthMetadata::parse(bytes.expose(), issuer)
                        .map_err(|_| OAuthHttpError::Document)?;
                    let scope = challenge
                        .as_ref()
                        .and_then(BearerChallenge::scope)
                        .map(str::to_owned)
                        .or_else(|| {
                            protected
                                .scopes()
                                .map(|values| values.join(" "))
                                .filter(|scope| !scope.is_empty())
                        });
                    return Ok(Discovered {
                        resource: protected,
                        authorization,
                        scope,
                    });
                }
                // A server fault remains a fault, never absence or permission
                // to substitute an unvalidated legacy authorization endpoint.
                if let Some(error) = unavailable {
                    return Err(error);
                }
            }
            Err(OAuthHttpError::MetadataMissing)
        })
        .await
    }

    async fn metadata(&self, url: Url) -> Result<Option<Secret<Vec<u8>>>, OAuthHttpError> {
        let url = https(url.as_str(), self.allow_loopback)?;
        let client = self.pinned_client(&url).await?;
        let response = client
            .get(url)
            .header(header::ACCEPT, "application/json")
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|_| OAuthHttpError::Transport)?;
        headers(response.headers())?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            return Ok(None);
        }
        if response.status() != StatusCode::OK {
            return Err(OAuthHttpError::Status(response.status().as_u16()));
        }
        document(response).await.map(Some)
    }

    pub async fn register(
        &self,
        metadata: &OAuthMetadata,
        redirect: &str,
        budget: Duration,
    ) -> Result<PublicClient, OAuthHttpError> {
        let endpoint = metadata
            .registration_endpoint()
            .ok_or(OAuthHttpError::RegistrationRequired)?;
        let request =
            PublicClient::registration_request(redirect).map_err(|_| OAuthHttpError::Document)?;
        timed(budget, async {
            let endpoint = https(endpoint.as_str(), self.allow_loopback)?;
            let client = self.pinned_client(&endpoint).await?;
            let response = client
                .post(endpoint)
                .header(header::ACCEPT, "application/json")
                .header(header::ACCEPT_ENCODING, "identity")
                .header(header::CONTENT_TYPE, "application/json")
                .body(request)
                .send()
                .await
                .map_err(|_| OAuthHttpError::Transport)?;
            headers(response.headers())?;
            if !matches!(response.status(), StatusCode::OK | StatusCode::CREATED) {
                return Err(OAuthHttpError::Status(response.status().as_u16()));
            }
            PublicClient::from_registration(&document(response).await?, redirect)
                .map_err(|_| OAuthHttpError::Document)
        })
        .await
    }

    pub async fn token(
        &self,
        metadata: &OAuthMetadata,
        form: &Secret<String>,
        budget: Duration,
    ) -> Result<Secret<Vec<u8>>, OAuthHttpError> {
        if form.expose().len() > 65536 {
            return Err(OAuthHttpError::Document);
        }
        timed(budget, async {
            let endpoint = https(metadata.token_endpoint().as_str(), self.allow_loopback)?;
            let client = self.pinned_client(&endpoint).await?;
            let response = client
                .post(endpoint)
                .header(header::ACCEPT, "application/json")
                .header(header::ACCEPT_ENCODING, "identity")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(form.expose().clone())
                .send()
                .await
                .map_err(|_| OAuthHttpError::Transport)?;
            headers(response.headers())?;
            let status = response.status();
            if status == StatusCode::OK {
                return document(response).await;
            }
            if matches!(status, StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED) {
                #[derive(serde::Deserialize)]
                struct Failure {
                    error: String,
                }
                if let Ok(bytes) = document(response).await {
                    if let Ok(failure) = serde_json::from_slice::<Failure>(bytes.expose()) {
                        if matches!(
                            failure.error.as_str(),
                            "invalid_grant" | "invalid_client" | "interaction_required"
                        ) {
                            return Err(OAuthHttpError::TokenRejected);
                        }
                    }
                }
            }
            Err(OAuthHttpError::Status(status.as_u16()))
        })
        .await
    }
}

fn https(raw: &str, allow_loopback: bool) -> Result<Url, OAuthHttpError> {
    if raw.len() > 8192 || raw.trim() != raw || raw.chars().any(char::is_control) {
        return Err(OAuthHttpError::Endpoint);
    }
    let url = Url::parse(raw).map_err(|_| OAuthHttpError::Endpoint)?;
    if url.scheme() != "https"
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(OAuthHttpError::Endpoint);
    }
    ssrf::validate_host(&url, allow_loopback).map_err(|()| OAuthHttpError::Endpoint)?;
    Ok(url)
}
fn headers(values: &header::HeaderMap) -> Result<(), OAuthHttpError> {
    if values.len() > 64
        || values
            .iter()
            .map(|(name, value)| name.as_str().len() + value.as_bytes().len())
            .sum::<usize>()
            > 16384
    {
        return Err(OAuthHttpError::Document);
    }
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_ENCODING,
        header::CONTENT_LENGTH,
    ] {
        if values.get_all(name).iter().count() > 1 {
            return Err(OAuthHttpError::Document);
        }
    }
    if values
        .get(header::CONTENT_ENCODING)
        .is_some_and(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
    {
        return Err(OAuthHttpError::Document);
    }
    Ok(())
}
async fn document(mut response: reqwest::Response) -> Result<Secret<Vec<u8>>, OAuthHttpError> {
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or(OAuthHttpError::Document)?;
    if !content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/json")
        || response.content_length().is_some_and(|bytes| bytes > 65536)
    {
        return Err(OAuthHttpError::Document);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| OAuthHttpError::Transport)?
    {
        if bytes.len().saturating_add(chunk.len()) > 65536 {
            return Err(OAuthHttpError::Document);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Secret::new(bytes))
}
async fn timed<T>(
    budget: Duration,
    operation: impl std::future::Future<Output = Result<T, OAuthHttpError>>,
) -> Result<T, OAuthHttpError> {
    if budget.is_zero() || budget > Duration::from_secs(120) {
        return Err(OAuthHttpError::TimedOut);
    }
    tokio::time::timeout(budget, operation)
        .await
        .map_err(|_| OAuthHttpError::TimedOut)?
}

#[cfg(test)]
#[path = "http/tests.rs"]
mod tests;

#[cfg(any(test, feature = "loopback-testkit"))]
#[doc(hidden)]
pub mod testkit;
