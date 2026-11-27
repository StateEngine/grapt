//! TLS and HTTP client construction, shared by every route family.
//!
//! Shared by the ChatGPT/Codex route and the other provider routes.
//! None of it is protocol: the `ring` provider, the refusal of redirects,
//! and proxies are shared by all providers. HTTPS endpoints also retain the
//! client's scheme guard. Only parsed literal loopback HTTP endpoints select
//! the local client, through the ordinary endpoint resolver.

use std::sync::Arc;
use std::time::Duration;

use crate::codex::{CodexDeadlines, CodexError};

/// Build the rustls configuration for one client.
///
/// The `ring` provider is constructed here and installed into
/// this one `ClientConfig`. `CryptoProvider::install_default` is **not** called
/// — a process-global provider is shared mutable state a library must not set
/// on its embedder's behalf, and a second caller installing a different one
/// would be a silent, order-dependent change of the TLS stack.
pub(crate) fn tls_config() -> Result<rustls::ClientConfig, CodexError> {
    use rustls_platform_verifier::BuilderVerifierExt;

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = match rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
    {
        Ok(builder) => builder,
        Err(error) => {
            return Err(CodexError::Tls {
                reason: error.to_string(),
            })
        }
    };
    match builder.with_platform_verifier() {
        Ok(builder) => {
            let mut config = builder.with_no_client_auth();
            // A preconfigured rustls client does not receive reqwest's ALPN
            // defaults. Negotiate HTTP/2 when supported, with HTTP/1.1 fallback.
            config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
            Ok(config)
        }
        Err(error) => Err(CodexError::Tls {
            reason: error.to_string(),
        }),
    }
}

/// The settings shared by production and by the loopback fixtures.
///
/// A lazy pool in `client_pool` is scoped to account, credential version and
/// route origin; authentication is still placed per request. Retain at most
/// one idle connection per host for 90 seconds. Redirects are refused
/// because a redirect would re-send the `Authorization` header to a host this
/// attempt never resolved. Proxies are refused because ambient proxy
/// configuration is exactly the egress dimension the capability lattice is
/// supposed to govern.
pub(crate) fn base_client_builder(deadlines: CodexDeadlines) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(deadlines.connect())
        .pool_max_idle_per_host(1)
        .pool_idle_timeout(Duration::from_secs(90))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
}

/// The client selected for HTTPS endpoints.
pub(crate) fn build_client(deadlines: CodexDeadlines) -> Result<reqwest::Client, CodexError> {
    let tls = tls_config()?;
    match base_client_builder(deadlines)
        .use_preconfigured_tls(tls)
        .https_only(true)
        .build()
    {
        Ok(client) => Ok(client),
        Err(error) => Err(CodexError::ClientBuild {
            reason: error.to_string(),
        }),
    }
}

/// Selected only after the route resolver admits literal loopback HTTP, or
/// by fixtures that already carry parsed loopback authority. Redirects and
/// proxies remain disabled, so the client cannot escape its bound endpoint.
pub(crate) fn build_loopback_client(
    deadlines: CodexDeadlines,
) -> Result<reqwest::Client, CodexError> {
    // The same TLS configuration as production — `rustls-no-provider` requires
    // one on every client, and building the loopback client identically means
    // the ONLY difference between the two paths is the `https_only(true)` line
    // production adds. Comparing these two functions exposes that difference.
    let tls = tls_config()?;
    match base_client_builder(deadlines)
        .use_preconfigured_tls(tls)
        .build()
    {
        Ok(client) => Ok(client),
        Err(error) => Err(CodexError::ClientBuild {
            reason: error.to_string(),
        }),
    }
}
