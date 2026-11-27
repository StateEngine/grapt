//! Public-only SSRF host and IP address policy for the plugin HTTP(S) broker.

use std::net::{IpAddr, SocketAddr};
use url::{Host, Url};

const BLOCKED_MSG: &str =
    "plugin_network_blocked: private, loopback, link-local and metadata addresses are denied";

/// Validate a URL's host against the public-only SSRF policy.
///
/// When `allow_loopback` is true (explicit loopback test fixtures only),
/// loopback hosts (`127.0.0.0/8`, `::1`, `localhost`) are admitted while
/// private, link-local/metadata, and reserved ranges remain blocked.
pub(crate) fn validate_host(url: &Url, allow_loopback: bool) -> Result<(), String> {
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
                return Err(BLOCKED_MSG.to_owned());
            }
            Ok(())
        }
        None => Err(BLOCKED_MSG.to_owned()),
    }
}

/// Validate a bounded list (1..=32) of DNS-resolved socket addresses against the SSRF policy.
pub(crate) fn validate_addresses(
    addresses: impl IntoIterator<Item = SocketAddr>,
    allow_loopback: bool,
) -> Result<Vec<SocketAddr>, String> {
    let mut pinned = Vec::new();
    for address in addresses.into_iter().take(33) {
        ensure_admitted_ip(address.ip(), allow_loopback)?;
        if pinned.len() == 32 {
            return Err("plugin_network_dns: lookup requires 1..32 addresses".to_owned());
        }
        pinned.push(address);
    }
    if pinned.is_empty() {
        return Err("plugin_network_dns: lookup requires 1..32 addresses".to_owned());
    }
    Ok(pinned)
}

pub(crate) fn ensure_admitted_ip(ip: IpAddr, allow_loopback: bool) -> Result<(), String> {
    if allow_loopback && ip.is_loopback() {
        return Ok(());
    }
    if is_public_ip(ip) {
        Ok(())
    } else {
        Err(BLOCKED_MSG.to_owned())
    }
}

/// Conservative subset of public unicast matching `grapt-web`'s SSRF policy.
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
