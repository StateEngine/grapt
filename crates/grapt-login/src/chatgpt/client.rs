//! The same bounded HTTPS policy used by the CLI sign-in adapter.
use std::sync::Arc;
use std::time::Duration;

use rustls_platform_verifier::BuilderVerifierExt;

use super::{SignInClient, SignInError};

pub fn production_client() -> Result<SignInClient, SignInError> {
    bounded_client(true)
}

pub(crate) fn bounded_client(https_only: bool) -> Result<SignInClient, SignInError> {
    const WHAT: &str = "the ChatGPT sign-in service";
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| SignInError::Request(WHAT))?
        .with_platform_verifier()
        .map_err(|_| SignInError::Request(WHAT))?
        .with_no_client_auth();
    SignInClient::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(0)
        .pool_idle_timeout(None)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(https_only)
        .use_preconfigured_tls(tls)
        .build()
        .map_err(|_| SignInError::Request(WHAT))
}

/// Explicit plaintext issuer fixture, unavailable in ordinary product builds.
#[cfg(feature = "loopback-testkit")]
pub fn loopback_client(issuer: &str) -> Result<SignInClient, SignInError> {
    if issuer.len() > 2048 {
        return Err(SignInError::Malformed("fixture issuer"));
    }
    let url = reqwest::Url::parse(issuer).map_err(|_| SignInError::Malformed("fixture issuer"))?;
    if url.scheme() != "http"
        || !url
            .host_str()
            .and_then(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|address| address.is_loopback())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SignInError::Malformed("fixture issuer"));
    }
    bounded_client(false)
}
