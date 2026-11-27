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
    resolve_pinned_with(url, allow_loopback, |host, port| {
        (host, port)
            .to_socket_addrs()
            .map(|iter| iter.collect())
            .map_err(|_| ())
    })
    .await
}

pub(crate) async fn resolve_pinned_with<F>(
    url: &Url,
    allow_loopback: bool,
    lookup: F,
) -> Result<(String, Vec<SocketAddr>), ()>
where
    F: FnOnce(&str, u16) -> Result<Vec<SocketAddr>, ()> + Send + 'static,
{
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
        let resolved = lookup(resolve_host.as_str(), port)?;
        grapt_web::validate_public_addresses(resolved, allow_loopback).map_err(|_| ())
    })
    .await
    .map_err(|_| ())??;
    Ok((host, addresses))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::http::{MetadataHttp, OAuthHttpError};
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn discover_refuses_non_public_dns_answers_before_transport(
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let loopback = listener.local_addr()?;

        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 443);
        let cases = [
            vec![loopback],
            vec![public, loopback],
            vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
                443,
            )],
            vec![
                public,
                SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 443),
            ],
            vec![SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443)],
            vec![SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
                443,
            )],
        ];

        for answers in cases {
            let client =
                MetadataHttp::new()?.with_dns_lookup(Arc::new(move |_, _| Ok(answers.clone())));
            let error = client
                .discover("https://rebind.example/mcp", Duration::from_secs(2))
                .await
                .err()
                .ok_or("expected SSRF refusal before transport")?;
            assert_eq!(error, OAuthHttpError::Endpoint);
        }

        // Prove no TCP connection reached the loopback listener.
        assert!(matches!(
            listener.accept(),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock
        ));
        Ok(())
    }
}
