//! DNS-pinned SSRF boundary for MCP OAuth HTTP requests, backed by
//! `grapt-web`'s single public-only host and IP policy.

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

pub(crate) fn validate_host(url: &Url, allow_loopback: bool) -> Result<(), ()> {
    grapt_web::validate_public_host(url, allow_loopback).map_err(|_| ())
}

/// Resolve `(host, port)` under a bounded concurrency slot and validate every
/// returned address against `grapt-web`'s SSRF policy before pinning.
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
        grapt_web::validate_public_addresses(resolved, allow_loopback).map_err(|_| ())
    })
    .await
    .map_err(|_| ())??;
    Ok((host, addresses))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::dns::Resolve;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::str::FromStr;

    #[test]
    fn non_public_dns_answers_are_rejected_even_when_mixed_with_public_ips() {
        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 443);
        for non_public in [
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 443),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 443),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), 443),
        ] {
            assert!(grapt_web::validate_public_addresses([non_public], false).is_err());
            assert!(grapt_web::validate_public_addresses([public, non_public], false).is_err());
        }
        assert!(grapt_web::validate_public_addresses([public], false).is_ok());
    }

    #[tokio::test]
    async fn pinned_dns_returns_only_validated_addresses_for_exact_host(
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let pinned_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 443);
        let resolver = PinnedDns::new("auth.example".to_owned(), vec![pinned_addr]);

        let resolved: Vec<SocketAddr> = resolver
            .resolve(dns::Name::from_str("auth.example")?)
            .await?
            .collect();
        assert_eq!(resolved, vec![pinned_addr]);

        let refused = resolver
            .resolve(dns::Name::from_str("other.example")?)
            .await;
        assert!(refused.is_err());
        Ok(())
    }
}
