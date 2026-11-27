// Copyright 2023-2026 SpaceXAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: connection-bound DNS validation, exact-origin redirects,
// one deadline across DNS/redirects/body, streaming limits, no ambient proxy.

use crate::{FetchError, WebUrl, MAX_BODY_BYTES};
use reqwest::{dns, header};
use rustls_platform_verifier::BuilderVerifierExt;
use std::{
    net::SocketAddr,
    sync::{Arc, OnceLock},
    time::Duration,
};

pub struct Page {
    pub url: WebUrl,
    pub media_type: String,
    pub text: String,
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum FetchOutcome {
    Page(Page),
    /// The caller must admit this destination separately before fetching it.
    Redirect(WebUrl),
}

/// DNS cannot fall back to a fresh lookup inside the HTTP client. Even an
/// unexpected resolver invocation fails instead of acquiring another host.
#[derive(Debug)]
struct PinnedDns {
    host: String,
    addresses: Vec<SocketAddr>,
}

impl dns::Resolve for PinnedDns {
    fn resolve(&self, name: dns::Name) -> dns::Resolving {
        let addresses = if name.as_str() == self.host {
            Some(self.addresses.clone())
        } else {
            None
        };
        Box::pin(async move {
            match addresses {
                Some(addresses) => Ok(Box::new(addresses.into_iter()) as dns::Addrs),
                None => Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "unbound web host",
                ))
                    as Box<dyn std::error::Error + Send + Sync>),
            }
        })
    }
}

fn tls_config() -> Result<&'static rustls::ClientConfig, FetchError> {
    static TLS: OnceLock<rustls::ClientConfig> = OnceLock::new();
    if let Some(config) = TLS.get() {
        return Ok(config);
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| FetchError::Client)?
        .with_platform_verifier()
        .map_err(|_| FetchError::Client)?
        .with_no_client_auth();
    // Independent fetches must still authenticate a full TLS handshake.
    config.resumption = rustls::client::Resumption::disabled();
    Ok(TLS.get_or_init(|| config))
}

fn client(url: &WebUrl, addresses: Vec<SocketAddr>) -> Result<reqwest::Client, FetchError> {
    reqwest::Client::builder()
        .use_preconfigured_tls(tls_config()?.clone())
        .dns_resolver(Arc::new(PinnedDns {
            host: url.host().to_owned(),
            addresses,
        }))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .pool_max_idle_per_host(0)
        .http2_max_header_list_size(16 * 1024)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()
        .map_err(|_| FetchError::Client)
}

/// Fetch only after the existing permission owner admits `url`'s exact origin.
/// Dropping this future cancels connection and body reads. An OS DNS lookup
/// may finish in the background while retaining one of four resolver slots.
pub async fn fetch(url: WebUrl, budget: Duration) -> Result<FetchOutcome, FetchError> {
    fetch_resolving(url, budget, async |url: &WebUrl| url.resolve().await).await
}

// The same fetch path accepts a resolver in private HTTP fixtures. Production
// always uses WebUrl::resolve, which checks every address before connecting.
async fn fetch_resolving(
    url: WebUrl,
    budget: Duration,
    resolve: impl AsyncFn(&WebUrl) -> Result<Vec<SocketAddr>, FetchError>,
) -> Result<FetchOutcome, FetchError> {
    if budget.is_zero() || budget > Duration::from_secs(60) {
        return Err(FetchError::Budget);
    }
    tokio::time::timeout(budget, fetch_inner(url, resolve))
        .await
        .map_err(|_| FetchError::TimedOut)?
}

async fn fetch_inner(
    mut url: WebUrl,
    resolve: impl AsyncFn(&WebUrl) -> Result<Vec<SocketAddr>, FetchError>,
) -> Result<FetchOutcome, FetchError> {
    for hop in 0..=10 {
        let addresses = resolve(&url).await?;
        let client = client(&url, addresses)?;
        let mut response = client.get(url.as_str())
            .header(header::USER_AGENT, "SE-Code/0.1")
            .header(header::ACCEPT, "text/markdown,text/html,application/xhtml+xml,text/plain;q=0.9,application/json;q=0.8")
            .header(header::ACCEPT_ENCODING, "identity")
            .send().await.map_err(|_| FetchError::Transport)?;
        validate_headers(response.headers())?;
        let status = response.status();
        if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(FetchError::Redirect)?;
            let next = url.redirect(location)?;
            if next.0.origin() != url.0.origin() {
                return Ok(FetchOutcome::Redirect(next));
            }
            if hop == 10 {
                return Err(FetchError::Redirect);
            }
            url = next;
            continue;
        }
        if !status.is_success() {
            return Err(FetchError::Status(status.as_u16()));
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity"))
        {
            return Err(FetchError::ContentType);
        }
        let media_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .ok_or(FetchError::ContentType)?
            .to_owned();
        crate::render::validate_media_type(&media_type)?;
        let bytes = read_body(&mut response).await?;
        let text = crate::render::render_async(bytes, media_type.clone()).await?;
        return Ok(FetchOutcome::Page(Page {
            url,
            media_type,
            text,
        }));
    }
    Err(FetchError::Redirect)
}

async fn read_body(response: &mut reqwest::Response) -> Result<Vec<u8>, FetchError> {
    if response
        .content_length()
        .is_some_and(|bytes| bytes > MAX_BODY_BYTES as u64)
    {
        return Err(FetchError::Oversized);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| FetchError::Transport)? {
        if chunk.len() > MAX_BODY_BYTES - bytes.len() {
            return Err(FetchError::Oversized);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn validate_headers(headers: &header::HeaderMap) -> Result<(), FetchError> {
    if headers.len() > 64
        || headers
            .iter()
            .map(|(name, value)| name.as_str().len() + value.len())
            .sum::<usize>()
            > 16 * 1024
    {
        return Err(FetchError::Headers);
    }
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_ENCODING,
        header::LOCATION,
        header::CONTENT_LENGTH,
    ] {
        if headers.get_all(name).iter().count() > 1 {
            return Err(FetchError::Headers);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
