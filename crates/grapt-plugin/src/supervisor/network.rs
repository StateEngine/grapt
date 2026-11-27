// Copyright 2023-2026 SpaceXAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: declaration-bound HTTP(S), pinned DNS, bounded payloads
// and generation retirement, without ambient proxy or automatic redirects.

//! Bounded HTTP(S) broker. The guest has no native socket authority.
use crate::PluginAccess;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use reqwest::{dns, header, Client, Method};
use rustls_platform_verifier::BuilderVerifierExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const BODY_MAX: usize = 128 * 1024;
const HEADERS_MAX: usize = 16 * 1024;
static DNS_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
static TLS: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchRequest {
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
}

#[derive(Debug)]
struct PinnedDns {
    host: String,
    addresses: Vec<SocketAddr>,
}
impl dns::Resolve for PinnedDns {
    fn resolve(&self, name: dns::Name) -> dns::Resolving {
        let addresses = (name.as_str() == self.host).then(|| self.addresses.clone());
        Box::pin(async move {
            match addresses {
                Some(addresses) => Ok(Box::new(addresses.into_iter()) as dns::Addrs),
                None => Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "plugin_network_dns_unbound",
                ))
                    as Box<dyn std::error::Error + Send + Sync>),
            }
        })
    }
}

pub(super) fn fetch(
    access: &PluginAccess,
    live: impl Fn() -> bool,
    payload: Value,
    frame: usize,
) -> Result<Value, String> {
    if !live() {
        return Err(
            "plugin_network_closed: this plugin call or generation is no longer live".into(),
        );
    }
    let request: FetchRequest = serde_json::from_value(payload)
        .map_err(|_| "plugin_network_invalid: malformed fetch request".to_owned())?;
    let url = access.admit_url(&request.url)?;
    let limit = BODY_MAX.min(frame / 8);
    if request.headers.len() > 64
        || request
            .headers
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>()
            > HEADERS_MAX
    {
        return Err("plugin_network_headers: at most 64 headers and 16 KiB are permitted".into());
    }
    let body = match request.body {
        Some(body) if body.len() <= limit.div_ceil(3) * 4 => STANDARD
            .decode(body)
            .map_err(|_| "plugin_network_invalid: malformed body encoding".to_owned())?,
        Some(_) => return Err("plugin_network_body: request body exceeds the broker bound".into()),
        None => Vec::new(),
    };
    if body.len() > limit {
        return Err("plugin_network_body: request body exceeds the broker bound".into());
    }
    let method = Method::from_bytes(request.method.as_bytes())
        .map_err(|_| "plugin_network_method: invalid HTTP method".to_owned())?;
    if ![
        Method::GET,
        Method::HEAD,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
    ]
    .contains(&method)
    {
        return Err("plugin_network_method: only ordinary HTTP methods are supported; CONNECT and protocol upgrades are denied".into());
    }
    let mut headers = header::HeaderMap::new();
    for (name, value) in request.headers {
        let name = header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| "plugin_network_headers: invalid header name".to_owned())?;
        if matches!(
            name.as_str(),
            "host"
                | "connection"
                | "upgrade"
                | "proxy-authorization"
                | "proxy-connection"
                | "transfer-encoding"
                | "content-length"
        ) {
            return Err(
                "plugin_network_headers: routing, proxy and upgrade headers are denied".into(),
            );
        }
        let value = header::HeaderValue::from_str(&value)
            .map_err(|_| "plugin_network_headers: invalid header value".to_owned())?;
        headers.append(name, value);
    }
    headers.insert(
        header::ACCEPT_ENCODING,
        header::HeaderValue::from_static("identity"),
    );
    let runtime = RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .max_blocking_threads(4)
                .enable_all()
                .build()
                .map_err(|_| {
                    "plugin_network_unavailable: broker runtime could not start".to_owned()
                })
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let allow_loopback = access.allows_loopback();
    runtime.block_on(async {
        let retired = async {
            while live() { tokio::time::sleep(Duration::from_millis(20)).await; }
            Err("plugin_network_closed: this plugin call or generation was retired".to_owned())
        };
        tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(30), exchange(url, method, headers, body, limit, allow_loopback)) =>
                result.map_err(|_| "plugin_network_timeout: HTTP exchange exceeded 30 seconds".to_owned())?,
            result = retired => result,
        }
    })
}

async fn exchange(
    url: url::Url,
    method: Method,
    headers: header::HeaderMap,
    body: Vec<u8>,
    limit: usize,
    allow_loopback: bool,
) -> Result<Value, String> {
    let host = url
        .host_str()
        .ok_or("plugin_network_invalid: missing host")?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = url
        .port_or_known_default()
        .ok_or("plugin_network_invalid: missing port")?;
    // The permit moves into the resolver worker: a timed-out OS lookup keeps
    // its slot until it actually exits, so cancellation cannot queue unbounded
    // background lookups. Every connected address belongs to this admitted host.
    let permit = DNS_SLOTS
        .acquire()
        .await
        .map_err(|_| "plugin_network_dns: resolver closed")?;
    let resolve_host = host.clone();
    let addresses = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let addresses = (resolve_host.as_str(), port)
            .to_socket_addrs()
            .map_err(|_| "plugin_network_dns: lookup failed".to_owned())?;
        crate::ssrf::validate_addresses(addresses, allow_loopback)
    })
    .await
    .map_err(|_| "plugin_network_dns: resolver worker failed".to_owned())??;
    let tls = TLS
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|_| "plugin_network_tls: protocol initialization failed".to_owned())?
                .with_platform_verifier()
                .map_err(|_| "plugin_network_tls: trust initialization failed".to_owned())?
                .with_no_client_auth();
            Ok(Arc::new(config))
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let client = Client::builder()
        .use_preconfigured_tls((**tls).clone())
        .dns_resolver(Arc::new(PinnedDns { host, addresses }))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .pool_max_idle_per_host(0)
        .http2_max_header_list_size(HEADERS_MAX as u32)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .build()
        .map_err(|_| "plugin_network_client: HTTP client could not start".to_owned())?;
    let mut response = client
        .request(method, url)
        .headers(headers)
        .body(body)
        .send()
        .await
        .map_err(|_| "plugin_network_transport: HTTP exchange failed".to_owned())?;
    if response.headers().len() > 64
        || response
            .headers()
            .iter()
            .map(|(name, value)| name.as_str().len() + value.len())
            .sum::<usize>()
            > HEADERS_MAX
    {
        return Err("plugin_network_headers: response exceeds the header bound".into());
    }
    if response.status() == reqwest::StatusCode::SWITCHING_PROTOCOLS {
        return Err("plugin_network_upgrade: protocol upgrades are denied".into());
    }
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            value
                .to_str()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
                .map_err(|_| "plugin_network_headers: response header is not text".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "plugin_network_transport: response body failed".to_owned())?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err("plugin_network_body: response body exceeds the broker bound".into());
        }
        body.extend_from_slice(&chunk);
    }
    // Redirects return to the bridge. Each new origin must be admitted again;
    // authentication headers are never forwarded by an automatic host redirect.
    Ok(json!({ "status": status, "headers": headers, "body": STANDARD.encode(body) }))
}
