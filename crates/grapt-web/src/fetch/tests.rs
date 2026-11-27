use super::*;
use crate::policy::validate_addresses;
use reqwest::dns::Resolve;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn mixed_dns_answers_and_overflow_are_refused() -> Result<(), Box<dyn std::error::Error>> {
    let public: SocketAddr = "8.8.8.8:443".parse()?;
    let private: SocketAddr = "127.0.0.1:443".parse()?;
    assert_eq!(
        validate_addresses([public, private]),
        Err(FetchError::Address)
    );
    assert_eq!(validate_addresses([]), Err(FetchError::Dns));
    assert_eq!(validate_addresses([public; 33]), Err(FetchError::Dns));
    assert_eq!(validate_addresses([public])?, vec![public]);
    Ok(())
}

#[test]
fn redirect_requires_exact_origin_and_revalidates_url() -> Result<(), FetchError> {
    let url = WebUrl::parse("https://example.com/Dir/Page")?;
    assert_eq!(
        url.redirect("../Other")?.as_str(),
        "https://example.com/Other"
    );
    for location in [
        "https://www.example.com/",
        "http://example.com/",
        "https://example.com:8443/",
    ] {
        assert_ne!(url.redirect(location)?.0.origin(), url.0.origin());
    }
    assert_eq!(url.redirect("http://127.0.0.1/"), Err(FetchError::Address));
    assert_eq!(
        url.redirect("https://u:p@example.com/"),
        Err(FetchError::Url)
    );
    Ok(())
}

#[tokio::test]
async fn pinned_resolver_cannot_acquire_another_host() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = "8.8.8.8:443".parse()?;
    let resolver = PinnedDns {
        host: "example.com".to_owned(),
        addresses: vec![addr],
    };
    assert_eq!(
        resolver
            .resolve("example.com".parse()?)
            .await
            .map_err(std::io::Error::other)?
            .collect::<Vec<_>>(),
        vec![addr]
    );
    assert!(resolver
        .resolve("other.example.com".parse()?)
        .await
        .is_err());
    Ok(())
}

/// Only this private client test injects a loopback address; public fetching
/// always enters through `WebUrl::resolve` and validates every returned IP.
async fn response(
    body: Vec<u8>,
    chunked: bool,
) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let n = stream.read(&mut buffer).await?;
            if n == 0 || request.len() + n > 8192 {
                return Err(std::io::Error::other("invalid fixture request"));
            }
            request.extend_from_slice(&buffer[..n]);
        }
        let header = if chunked {
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_owned()
        } else {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
        };
        stream.write_all(header.as_bytes()).await?;
        if chunked {
            for chunk in body.chunks(4096) {
                stream
                    .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                    .await?;
                stream.write_all(chunk).await?;
                stream.write_all(b"\r\n").await?;
            }
            stream.write_all(b"0\r\n\r\n").await?;
        } else {
            stream.write_all(&body).await?;
        }
        Ok::<_, std::io::Error>(())
    });
    let url = WebUrl::parse(&format!("http://example.com:{}/", address.port()))?;
    Ok(client(&url, vec![address])?
        .get(url.as_str())
        .send()
        .await?)
}

#[tokio::test]
async fn actual_connection_uses_pinned_addresses_and_body_is_bounded(
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut small = response(b"fixture body".to_vec(), false).await?;
        assert_eq!(read_body(&mut small).await?, b"fixture body");
        let mut exact = response(vec![b'x'; MAX_BODY_BYTES], true).await?;
        assert_eq!(read_body(&mut exact).await?.len(), MAX_BODY_BYTES);
        let mut oversized = response(vec![b'x'; MAX_BODY_BYTES + 1], true).await?;
        assert_eq!(read_body(&mut oversized).await, Err(FetchError::Oversized));
        let mut advertised = response(vec![b'x'; MAX_BODY_BYTES + 1], false).await?;
        assert_eq!(read_body(&mut advertised).await, Err(FetchError::Oversized));
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await?
}

#[tokio::test]
async fn invalid_budget_fails_without_dns() -> Result<(), FetchError> {
    let url = WebUrl::parse("https://example.com/")?;
    assert_eq!(
        fetch(url.clone(), Duration::ZERO).await.err(),
        Some(FetchError::Budget)
    );
    assert_eq!(
        fetch(url, Duration::from_secs(61)).await.err(),
        Some(FetchError::Budget)
    );
    Ok(())
}

#[test]
fn ambiguous_and_large_headers_are_rejected() {
    let mut headers = header::HeaderMap::new();
    headers.append(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain"),
    );
    headers.append(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/html"),
    );
    assert_eq!(validate_headers(&headers), Err(FetchError::Headers));
}

/// Runs the production redirect/body/deadline path against a synthetic public
/// name resolved to a loopback fixture; no public network service is contacted.
#[tokio::test]
async fn cross_origin_redirect_stops_before_resolving_the_new_destination(
) -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for location in [
        "https://other.example.com/secret",
        "http://example.com:8443/secret",
        "http://127.0.0.1/secret",
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let location = location.to_owned();
        let expected_private = location.contains("127.0.0.1");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let mut buffer = [0; 8192];
            let _ = stream.read(&mut buffer).await?;
            stream.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await
        });
        let resolves = AtomicUsize::new(0);
        let url = WebUrl::parse(&format!("http://example.com:{}/start", address.port()))?;
        let outcome = fetch_resolving(url, Duration::from_secs(3), async |_url: &WebUrl| {
            resolves.fetch_add(1, Ordering::SeqCst);
            Ok(vec![address])
        })
        .await;
        tokio::time::timeout(Duration::from_secs(3), server).await???;
        if expected_private {
            assert_eq!(outcome.err(), Some(FetchError::Address));
        } else {
            assert!(matches!(outcome, Ok(FetchOutcome::Redirect(_))));
        }
        assert_eq!(resolves.load(Ordering::SeqCst), 1);
    }
    Ok(())
}

#[tokio::test]
async fn stalled_body_obeys_the_original_fetch_deadline() -> Result<(), Box<dyn std::error::Error>>
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut buffer = [0; 8192];
        let _ = stream.read(&mut buffer).await?;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nx").await?;
        let _ = stopped.await;
        Ok::<_, std::io::Error>(())
    });
    let url = WebUrl::parse(&format!("http://example.com:{}/", address.port()))?;
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        fetch_resolving(url, Duration::from_secs(1), async |_url: &WebUrl| {
            Ok(vec![address])
        }),
    )
    .await?;
    let _ = stop.send(());
    tokio::time::timeout(Duration::from_secs(3), server).await???;
    assert_eq!(outcome.err(), Some(FetchError::TimedOut));
    Ok(())
}

#[test]
fn fetched_page_debug_does_not_expose_query_credentials_or_payload() -> Result<(), FetchError> {
    let page = Page {
        url: WebUrl::parse("https://example.com/private?token=query-secret")?,
        media_type: "text/plain; opaque=header-secret".into(),
        text: "payload-secret".into(),
    };
    let debug = format!("{:?}", FetchOutcome::Page(page));
    for secret in [
        "query-secret",
        "payload-secret",
        "header-secret",
        "/private",
    ] {
        assert!(!debug.contains(secret));
    }
    assert!(debug.contains("https://example.com"));
    Ok(())
}
