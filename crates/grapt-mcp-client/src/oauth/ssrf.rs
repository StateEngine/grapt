//! DNS-pinned SSRF boundary for MCP OAuth HTTP requests, backed by
//! `grapt-web`'s single public-only host and IP policy.

use reqwest::dns;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use url::{Host, Url};

static DNS_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
const MAX_DNS_ANSWERS: usize = 32;

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
            .map(|iter| iter.take(MAX_DNS_ANSWERS + 1).collect())
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
    use crate::oauth::http::testkit::{client as loopback_client, Reply, Server};
    use crate::oauth::http::{MetadataHttp, OAuthHttpError};
    use reqwest::dns::Resolve;
    use serde_json::json;
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
    use std::str::FromStr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[tokio::test]
    async fn pinned_dns_returns_only_validated_addresses_for_exact_host(
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let pinned_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)), 443);
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

    #[tokio::test]
    async fn discover_refuses_non_public_dns_answers_before_transport(
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let loopback = listener.local_addr()?;

        let doc_public = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1)), 443);
        let cases = [
            vec![loopback],
            vec![doc_public, loopback],
            vec![SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
                443,
            )],
            vec![
                doc_public,
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

    #[tokio::test]
    async fn discover_dials_injected_pinned_dns_address_for_domain_host(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // 1) Successful HTTPS discovery against `localhost` via injected PinnedDns.
        let server = Server::new(3, |base, request| {
            let host_base = base.replacen("127.0.0.1", "localhost", 1);
            match request.path.as_str() {
                "/mcp" => Reply {
                    status: "401 Unauthorized",
                    headers: vec![(
                        "WWW-Authenticate".into(),
                        format!("Bearer resource_metadata=\"{host_base}/prm\", scope=\"read\""),
                    )],
                    body: Vec::new(),
                    delay: Duration::ZERO,
                },
                "/prm" => Reply::json(json!({
                    "resource": format!("{host_base}/mcp"),
                    "authorization_servers": [format!("{host_base}/tenant")],
                })),
                "/.well-known/oauth-authorization-server/tenant" => Reply::json(json!({
                    "issuer": format!("{host_base}/tenant"),
                    "authorization_endpoint": format!("{host_base}/authorize"),
                    "token_endpoint": format!("{host_base}/token"),
                    "code_challenge_methods_supported": ["S256"],
                })),
                _ => Reply {
                    status: "404 Not Found",
                    ..Reply::json(json!({}))
                },
            }
        })?;
        let port = Url::parse(&server.base)?
            .port()
            .ok_or("fixture server port")?;
        let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let lookups = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&lookups);
        let client = loopback_client()?.with_dns_lookup(Arc::new(move |host, req_port| {
            if host == "localhost" && req_port == port {
                counted.fetch_add(1, Ordering::AcqRel);
                Ok(vec![target])
            } else {
                Err(())
            }
        }));
        let discovered = client
            .discover(
                &format!("https://localhost:{port}/mcp"),
                Duration::from_secs(10),
            )
            .await?;
        assert_eq!(discovered.scope.as_deref(), Some("read"));
        assert!(lookups.load(Ordering::Acquire) >= 3);
        server.finish()?;

        // 2) Prove reqwest uses PinnedDns (not OS DNS) by dialing an unresolvable
        // `.example` hostname whose pinned address points at a local listener.
        let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        probe.set_nonblocking(true)?;
        let probe_addr = probe.local_addr()?;
        let probe_port = probe_addr.port();
        let accepted = std::thread::spawn(move || {
            for _ in 0..200 {
                match probe.accept() {
                    Ok(_) => return true,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return false,
                }
            }
            false
        });
        let probe_client = loopback_client()?.with_dns_lookup(Arc::new(move |host, req_port| {
            if host == "pinned.example" && req_port == probe_port {
                Ok(vec![probe_addr])
            } else {
                Err(())
            }
        }));
        let _ = probe_client
            .discover(
                &format!("https://pinned.example:{probe_port}/mcp"),
                Duration::from_secs(5),
            )
            .await;
        assert!(
            accepted.join().map_err(|_| "probe thread panicked")?,
            "reqwest did not dial the PinnedDns-injected address for pinned.example"
        );
        Ok(())
    }
}
