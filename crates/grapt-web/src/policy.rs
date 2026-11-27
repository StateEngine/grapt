// Copyright 2023-2026 SpaceXAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: exact-origin redirects, public-only network policy,
// bounded DNS answers pinned to the actual connection, and redacted Debug.

use crate::FetchError;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use url::{Host, Url};

// A cancelled system lookup can continue in the OS resolver. Its permit
// therefore belongs to the blocking worker until that lookup actually exits.
// Repeated timeouts cannot accumulate unbounded background DNS jobs.
static DNS_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

#[derive(Clone, PartialEq, Eq)]
pub struct WebUrl(pub(crate) Url);

impl std::fmt::Debug for WebUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("WebUrl").field(&self.origin()).finish()
    }
}

impl WebUrl {
    pub fn parse(raw: &str) -> Result<Self, FetchError> {
        if raw.len() > 8192 || raw.chars().any(char::is_control) {
            return Err(FetchError::Url);
        }
        let mut url = Url::parse(raw).map_err(|_| FetchError::Url)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(FetchError::Url);
        }
        match url.host() {
            Some(Host::Ipv4(ip)) => ensure_public(IpAddr::V4(ip))?,
            Some(Host::Ipv6(ip)) => ensure_public(IpAddr::V6(ip))?,
            Some(Host::Domain(_)) => validate_public_host(&url, false)?,
            None => return Err(FetchError::Url),
        }
        url.set_fragment(None);
        Ok(Self(url))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
    pub fn scheme(&self) -> &str {
        self.0.scheme()
    }
    pub fn host(&self) -> &str {
        self.0.host_str().unwrap_or_default()
    }
    pub fn port(&self) -> u16 {
        self.0.port_or_known_default().unwrap_or(443)
    }
    pub fn origin(&self) -> String {
        self.0.origin().ascii_serialization()
    }

    pub(crate) fn redirect(&self, location: &str) -> Result<Self, FetchError> {
        if location.len() > 8192 || location.chars().any(char::is_control) {
            return Err(FetchError::Redirect);
        }
        let next = self.0.join(location).map_err(|_| FetchError::Redirect)?;
        Self::parse(next.as_str())
    }

    pub(crate) async fn resolve(&self) -> Result<Vec<SocketAddr>, FetchError> {
        match self.0.host() {
            Some(Host::Ipv4(ip)) => return Ok(vec![SocketAddr::new(IpAddr::V4(ip), self.port())]),
            Some(Host::Ipv6(ip)) => return Ok(vec![SocketAddr::new(IpAddr::V6(ip), self.port())]),
            _ => {}
        }
        let permit = DNS_SLOTS.acquire().await.map_err(|_| FetchError::Dns)?;
        let host = self.host().to_owned();
        let port = self.port();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let addresses = (host.as_str(), port)
                .to_socket_addrs()
                .map_err(|_| FetchError::Dns)?;
            validate_addresses(addresses)
        })
        .await
        .map_err(|_| FetchError::Dns)?
    }
}

/// Validate a URL host against the public-only SSRF policy.
///
/// When `allow_loopback` is true (explicit loopback test fixtures only),
/// loopback hosts (`127.0.0.0/8`, `::1`, `localhost`) are admitted while
/// private, link-local/metadata, and reserved ranges remain blocked.
pub fn validate_public_host(url: &Url, allow_loopback: bool) -> Result<(), FetchError> {
    match url.host() {
        Some(Host::Ipv4(ip)) => ensure_admitted_ip(IpAddr::V4(ip), allow_loopback),
        Some(Host::Ipv6(ip)) => ensure_admitted_ip(IpAddr::V6(ip), allow_loopback),
        Some(Host::Domain(host)) => {
            let host = host.trim_end_matches('.');
            if allow_loopback && host.eq_ignore_ascii_case("localhost") {
                return Ok(());
            }
            if !host.contains('.')
                || [
                    "localhost",
                    "local",
                    "internal",
                    "home.arpa",
                    "test",
                    "invalid",
                    "onion",
                ]
                .iter()
                .any(|suffix| {
                    host.eq_ignore_ascii_case(suffix)
                        || host.to_ascii_lowercase().ends_with(&format!(".{suffix}"))
                })
            {
                return Err(FetchError::Url);
            }
            Ok(())
        }
        None => Err(FetchError::Url),
    }
}

/// Validate a bounded list (1..=32) of DNS-resolved socket addresses against
/// the public-only SSRF policy.
pub fn validate_public_addresses(
    addresses: impl IntoIterator<Item = SocketAddr>,
    allow_loopback: bool,
) -> Result<Vec<SocketAddr>, FetchError> {
    let mut pinned = Vec::new();
    for address in addresses.into_iter().take(33) {
        ensure_admitted_ip(address.ip(), allow_loopback)?;
        if pinned.len() == 32 {
            return Err(FetchError::Dns);
        }
        pinned.push(address);
    }
    if pinned.is_empty() {
        return Err(FetchError::Dns);
    }
    Ok(pinned)
}

pub(crate) fn validate_addresses(
    addresses: impl IntoIterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, FetchError> {
    validate_public_addresses(addresses, false)
}

fn ensure_admitted_ip(ip: IpAddr, allow_loopback: bool) -> Result<(), FetchError> {
    if allow_loopback && ip.is_loopback() {
        return Ok(());
    }
    ensure_public(ip)
}

/// Conservative subset of public unicast. Special-purpose ranges are excluded
/// even where IANA permits individual globally reachable exceptions.
/// IANA IPv4/IPv6 special-purpose registries, reviewed 2026-10-15.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(matches!(a, 0 | 10 | 127 | 224..=255)
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 0 && matches!(c, 0 | 2) || b == 168 || b == 88 && c == 99))
                || (a == 198 && (matches!(b, 18 | 19) || b == 51 && c == 100))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            (0x2000..=0x3fff).contains(&s[0])
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && s[0] != 0x2002
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}

pub(crate) fn ensure_public(ip: IpAddr) -> Result<(), FetchError> {
    if is_public_ip(ip) {
        Ok(())
    } else {
        Err(FetchError::Address)
    }
}
