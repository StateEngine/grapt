//! Public-only SSRF host and IP address policy for MCP OAuth HTTP discovery
//! and token requests, with bounded DNS resolution pinned to the connection.

use reqwest::dns;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use url::{Host, Url};

static DNS_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

#[derive(Debug)]
pub(crate) struct PinnedDns {
    host: String,
    addresses: Vec<SocketAddr>,
}

impl PinnedDns {
    pub(crate) fn new(host: String, addresses: Vec<SocketAddr>) -> Self {
        Self { host, addresses }
    }
}

impl dns::Resolve for PinnedDns {
    fn resolve(&self, name: dns::Name) -> dns::Resolving {
        let addresses = (name.as_str() == self.host).then(|| self.addresses.clone());
        Box::pin(async move {
            match addresses {
                Some(addresses) => Ok(Box::new(addresses.into_iter()) as dns::Addrs),
                None => Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "oauth_dns_unbound",
                ))
                    as Box<dyn std::error::Error + Send + Sync>),
            }
        })
    }
}

/// Reject private, loopback, link-local, metadata, and reserved hosts/IPs.
///
/// When `allow_loopback` is true (explicit loopback test fixtures only),
/// loopback IPs (`127.0.0.0/8`, `::1`) are admitted while all other non-public
/// ranges (private RFC1918, link-local / cloud metadata `169.254.0.0/16`, ULA)
/// remain blocked.
pub(crate) fn validate_host(url: &Url, allow_loopback: bool) -> Result<(), ()> {
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
                return Err(());
            }
            Ok(())
        }
        None => Err(()),
    }
}

/// Resolve `(host, port)` under a bounded concurrency slot and validate every
/// returned address against the SSRF IP policy before pinning.
pub(crate) async fn resolve_pinned(
    url: &Url,
    allow_loopback: bool,
) -> Result<(String, Vec<SocketAddr>), ()> {
    validate_host(url, allow_loopback)?;
    let port = url.port_or_known_default().unwrap_or(443);
    match url.host() {
        Some(Host::Ipv4(ip)) => {
            let addr = SocketAddr::new(IpAddr::V4(ip), port);
            return Ok((ip.to_string(), vec![addr]));
        }
        Some(Host::Ipv6(ip)) => {
            let addr = SocketAddr::new(IpAddr::V6(ip), port);
            return Ok((ip.to_string(), vec![addr]));
        }
        Some(Host::Domain(_)) => {}
        None => return Err(()),
    }

    let host = url.host_str().ok_or(())?.to_owned();
    let resolve_host = host.clone();
    let permit = DNS_SLOTS.acquire().await.map_err(|_| ())?;
    let addresses = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let resolved = (resolve_host.as_str(), port)
            .to_socket_addrs()
            .map_err(|_| ())?;
        validate_addresses(resolved, allow_loopback)
    })
    .await
    .map_err(|_| ())??;
    Ok((host, addresses))
}

pub(crate) fn validate_addresses(
    addresses: impl IntoIterator<Item = SocketAddr>,
    allow_loopback: bool,
) -> Result<Vec<SocketAddr>, ()> {
    let mut pinned = Vec::new();
    for address in addresses.into_iter().take(33) {
        ensure_admitted_ip(address.ip(), allow_loopback)?;
        if pinned.len() == 32 {
            return Err(());
        }
        pinned.push(address);
    }
    if pinned.is_empty() {
        return Err(());
    }
    Ok(pinned)
}

pub(crate) fn ensure_admitted_ip(ip: IpAddr, allow_loopback: bool) -> Result<(), ()> {
    if allow_loopback && ip.is_loopback() {
        return Ok(());
    }
    if is_public_ip(ip) {
        Ok(())
    } else {
        Err(())
    }
}

/// Conservative subset of public unicast matching `grapt-web`'s SSRF policy.
/// Excludes private, loopback, link-local/metadata, CGNAT, multicast, and reserved ranges.
pub(crate) fn is_public_ip(ip: IpAddr) -> bool {
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
