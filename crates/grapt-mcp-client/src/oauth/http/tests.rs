use super::testkit::{client, Reply, Server};
use super::*;
use serde_json::json;
use std::net::TcpListener;
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn metadata(base: &str) -> Result<OAuthMetadata, OAuthErrorForTest> {
    let issuer = format!("{base}/tenant");
    OAuthMetadata::parse(json!({"issuer":issuer,"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"registration_endpoint":format!("{base}/register"),"code_challenge_methods_supported":["S256"],"authorization_response_iss_parameter_supported":true}).to_string().as_bytes(),&issuer)
}
type OAuthErrorForTest = crate::oauth::OAuthError;

#[tokio::test]
async fn actual_tls_discovery_registration_and_token_exchange_use_no_resource_credentials(
) -> TestResult {
    const REDIRECT: &str =
        "http://127.0.0.1:39281/mcp/oauth/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let server = Server::new(6, |base, request| match request.path.as_str() {
        "/mcp" => Reply {
            status: "401 Unauthorized",
            headers: vec![(
                "WWW-Authenticate".into(),
                format!("Bearer resource_metadata=\"{base}/prm\", scope=\"read write\""),
            )],
            body: Vec::new(),
            delay: Duration::ZERO,
        },
        "/prm" => Reply::json(
            json!({"resource":format!("{base}/mcp"),"authorization_servers":[format!("{base}/tenant")],"scopes_supported":["less"]}),
        ),
        "/.well-known/oauth-authorization-server/tenant" => Reply {
            status: "404 Not Found",
            ..Reply::json(json!({}))
        },
        "/.well-known/openid-configuration/tenant" => Reply::json(
            json!({"issuer":format!("{base}/tenant"),"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"registration_endpoint":format!("{base}/register"),"code_challenge_methods_supported":["S256"]}),
        ),
        "/register" => Reply {
            status: "201 Created",
            ..Reply::json(
                json!({"client_id":"fixture-client","redirect_uris":[REDIRECT],"token_endpoint_auth_method":"none"}),
            )
        },
        "/token" => Reply::json(
            json!({"access_token":"fixture-access","token_type":"Bearer","refresh_token":"fixture-refresh","expires_in":60}),
        ),
        _ => Reply {
            status: "400 Bad Request",
            ..Reply::json(json!({}))
        },
    })?;
    let client = client()?;
    let discovered = client
        .discover(&format!("{}/mcp", server.base), Duration::from_secs(15))
        .await?;
    assert_eq!(discovered.scope.as_deref(), Some("read write"));
    let registered = client
        .register(&discovered.authorization, REDIRECT, Duration::from_secs(5))
        .await?;
    assert_eq!(registered.client_id().expose(), "fixture-client");
    let response = client
        .token(
            &discovered.authorization,
            &Secret::new("grant_type=authorization_code&code=PRIVATE_FIXTURE".into()),
            Duration::from_secs(5),
        )
        .await?;
    assert!(response
        .expose()
        .windows("fixture-access".len())
        .any(|value| value == b"fixture-access"));
    {
        let seen = server.seen.lock().map_err(|_| "record lock")?;
        assert_eq!(seen.len(), 6);
        assert!(seen
            .iter()
            .all(|request| !request.authorization && !request.cookie));
        assert_eq!(seen[4].method, "POST");
        assert!(seen[5]
            .body
            .expose()
            .windows("PRIVATE_FIXTURE".len())
            .any(|value| value == b"PRIVATE_FIXTURE"));
    }
    server.finish()
}

#[tokio::test]
async fn actual_tls_token_redirect_is_not_followed_and_provider_errors_are_not_reflected(
) -> TestResult {
    let trap = TcpListener::bind("127.0.0.1:0")?;
    trap.set_nonblocking(true)?;
    let destination = format!("https://{}/steal", trap.local_addr()?);
    let server = Server::new(1, move |_, _| Reply {
        status: "307 Temporary Redirect",
        headers: vec![("Location".into(), destination.clone())],
        body: Vec::new(),
        delay: Duration::ZERO,
    })?;
    let error = client()?
        .token(
            &metadata(&server.base)?,
            &Secret::new("code=PRIVATE_CODE".into()),
            Duration::from_secs(5),
        )
        .await;
    assert!(matches!(error, Err(OAuthHttpError::Status(307))));
    assert!(matches!(trap.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock));
    server.finish()?;
    let server = Server::new(1, |_, _| Reply {
        status: "400 Bad Request",
        ..Reply::json(json!({"error":"invalid_grant","error_description":"PRIVATE_PROVIDER_WORDS"}))
    })?;
    let result = client()?
        .token(
            &metadata(&server.base)?,
            &Secret::new("refresh_token=PRIVATE_REFRESH".into()),
            Duration::from_secs(5),
        )
        .await;
    assert!(matches!(result, Err(OAuthHttpError::TokenRejected)));
    assert!(!format!("{result:?}").contains("PRIVATE_PROVIDER_WORDS"));
    server.finish()
}

#[tokio::test]
async fn actual_tls_stalled_token_body_keeps_the_original_deadline() -> TestResult {
    let server = Server::new(1, |_, _| Reply {
        delay: Duration::from_millis(1500),
        ..Reply::json(json!({"access_token":"fixture-access","token_type":"Bearer"}))
    })?;
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        client()?.token(
            &metadata(&server.base)?,
            &Secret::new("refresh_token=fixture".into()),
            Duration::from_millis(500),
        ),
    )
    .await?;
    assert!(matches!(result, Err(OAuthHttpError::TimedOut)));
    assert_eq!(
        server.seen.lock().map_err(|_| "record lock")?.len(),
        1,
        "deadline expired before the intended body witness executed"
    );
    server.finish()
}

#[tokio::test]
async fn discovery_does_not_substitute_a_different_issuer_or_call_its_token_endpoint() -> TestResult
{
    let server = Server::new(3, |base, request| match request.path.as_str() {
        "/mcp" => Reply {
            status: "401 Unauthorized",
            headers: vec![(
                "WWW-Authenticate".into(),
                format!("Bearer resource_metadata=\"{base}/prm\""),
            )],
            body: Vec::new(),
            delay: Duration::ZERO,
        },
        "/prm" => Reply::json(
            json!({"resource":format!("{base}/mcp"),"authorization_servers":[format!("{base}/tenant")]}),
        ),
        _ => Reply::json(
            json!({"issuer":format!("{base}/another-tenant"),"authorization_endpoint":format!("{base}/authorize"),"token_endpoint":format!("{base}/token"),"code_challenge_methods_supported":["S256"]}),
        ),
    })?;
    let result = client()?
        .discover(&format!("{}/mcp", server.base), Duration::from_secs(10))
        .await;
    assert!(matches!(result, Err(OAuthHttpError::Document)));
    assert!(server
        .seen
        .lock()
        .map_err(|_| "record lock")?
        .iter()
        .all(|request| request.method == "GET"));
    server.finish()
}

#[tokio::test]
async fn discovery_server_fault_is_not_relabelled_as_absent_metadata() -> TestResult {
    let server = Server::new(5, |base, request| match request.path.as_str() {
        "/mcp" => Reply {
            status: "401 Unauthorized",
            headers: vec![(
                "WWW-Authenticate".into(),
                format!("Bearer resource_metadata=\"{base}/prm\""),
            )],
            body: Vec::new(),
            delay: Duration::ZERO,
        },
        "/prm" => Reply::json(
            json!({"resource":format!("{base}/mcp"),"authorization_servers":[format!("{base}/tenant")]}),
        ),
        "/.well-known/oauth-authorization-server/tenant" => Reply {
            status: "503 Service Unavailable",
            ..Reply::json(json!({}))
        },
        _ => Reply {
            status: "404 Not Found",
            ..Reply::json(json!({}))
        },
    })?;
    let result = client()?
        .discover(&format!("{}/mcp", server.base), Duration::from_secs(10))
        .await;
    assert!(matches!(result, Err(OAuthHttpError::Status(503))));
    server.finish()
}

#[tokio::test]
async fn oversized_encoded_and_ambiguous_token_responses_are_refused_before_exposure() -> TestResult
{
    let server = Server::new(3, |_, request| {
        if request.body.expose() == b"case=oversized" {
            Reply {
                body: vec![b'x'; 65537],
                ..Reply::json(json!({}))
            }
        } else if request.body.expose() == b"case=encoded" {
            let mut reply = Reply::json(json!({}));
            reply
                .headers
                .push(("Content-Encoding".into(), "gzip".into()));
            reply
        } else {
            let mut reply = Reply::json(json!({}));
            reply
                .headers
                .push(("Content-Type".into(), "text/plain".into()));
            reply
        }
    })?;
    let client = client()?;
    let metadata = metadata(&server.base)?;
    for case in ["oversized", "encoded", "duplicate"] {
        let result = client
            .token(
                &metadata,
                &Secret::new(format!("case={case}")),
                Duration::from_secs(5),
            )
            .await;
        assert!(
            matches!(result, Err(OAuthHttpError::Document)),
            "invalid token response was accepted"
        );
    }
    server.finish()
}

#[tokio::test]
async fn production_discovery_blocks_private_loopback_link_local_and_metadata_hosts() -> TestResult
{
    use std::net::{Ipv4Addr, Ipv6Addr};
    let metadata_v4 = Ipv4Addr::new(169, 254, 169, 254);
    let rfc1918_10 = Ipv4Addr::new(10, 0, 0, 1);
    let rfc1918_172 = Ipv4Addr::new(172, 16, 0, 1);
    let rfc1918_192 = Ipv4Addr::new(192, 168, 1, 1);
    let ula_v6 = Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 1);
    let link_local_v6 = Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1);

    let prod = MetadataHttp::new()?;
    for blocked in [
        format!("https://{}:443/mcp", Ipv4Addr::LOCALHOST),
        format!("https://[{}]:443/mcp", Ipv6Addr::LOCALHOST),
        format!("https://{metadata_v4}/latest/meta-data/"),
        format!("https://{rfc1918_10}/mcp"),
        format!("https://{rfc1918_172}/mcp"),
        format!("https://{rfc1918_192}/mcp"),
        "https://metadata.example.internal/computeMetadata/v1/".to_owned(),
        "https://service.localhost/mcp".to_owned(),
    ] {
        let error = prod
            .discover(&blocked, Duration::from_secs(2))
            .await
            .err()
            .ok_or_else(|| format!("expected SSRF refusal for {blocked}"))?;
        assert_eq!(error, OAuthHttpError::Endpoint, "for {blocked}");
    }

    let loopback = client()?;
    for still_blocked in [
        format!("https://{metadata_v4}/latest/meta-data/"),
        format!("https://{rfc1918_10}/mcp"),
        format!("https://{rfc1918_192}/mcp"),
        format!("https://[{ula_v6}]/mcp"),
        format!("https://[{link_local_v6}]/mcp"),
    ] {
        let error = loopback
            .discover(&still_blocked, Duration::from_secs(2))
            .await
            .err()
            .ok_or_else(|| format!("expected SSRF refusal for {still_blocked}"))?;
        assert_eq!(error, OAuthHttpError::Endpoint, "for {still_blocked}");
    }
    Ok(())
}
