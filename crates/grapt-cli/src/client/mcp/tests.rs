use super::*;
use crate::client::{production_client, Allowance, ClientEndpoint, Credential};
use crate::CliLimits;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn exchange(
    server: &str,
    request: McpAuthRequestDto,
    status: u16,
    body: Vec<u8>,
) -> Result<(Result<McpAuthResponseDto, ApiClientError>, String), Box<dyn std::error::Error>> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint =
        ClientEndpoint::attributed_loopback(&format!("http://{}/", listener.local_addr()?))?;
    let http = production_client(false, Allowance::Request(Duration::from_secs(2)))?;
    let client = ApiClient {
        endpoint,
        client: http.clone(),
        event_client: http,
        basic: Some(Arc::new(Credential {
            username: "test".to_owned(),
            secret: "test".to_owned(),
        })),
        limits: CliLimits::production(),
        embedded: None,
        streak: Arc::new(std::sync::Mutex::new(Default::default())),
    };
    let peer = async {
        let (mut stream, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        let end = loop {
            if let Some(position) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                break position + 4;
            }
            if bytes.len() >= 16384 {
                return Err("request headers exceeded fixture bound".into());
            }
            let mut chunk = [0; 1024];
            let count = stream.read(&mut chunk).await?;
            if count == 0 {
                return Err("request ended before headers".into());
            }
            bytes.extend_from_slice(&chunk[..count]);
        };
        let header = std::str::from_utf8(&bytes[..end])?;
        let length: usize = header
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse())
            })
            .ok_or("no content length")??;
        if length > 65536 {
            return Err("request body exceeded fixture bound".into());
        }
        while bytes.len() < end + length {
            let mut chunk = [0; 1024];
            let count = stream.read(&mut chunk).await?;
            if count == 0 {
                return Err("request ended before body".into());
            }
            bytes.extend_from_slice(&chunk[..count]);
        }
        let headers = format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/credential-leak\r\nConnection: close\r\n\r\n", body.len());
        stream.write_all(headers.as_bytes()).await?;
        stream.write_all(&body).await?;
        Ok::<_, Box<dyn std::error::Error>>(String::from_utf8(bytes)?)
    };
    let operation = client.mcp_auth(server, Some("/repo with space"), &request);
    let (response, captured) = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::join!(operation, peer)
    })
    .await?;
    Ok((response, captured?))
}

#[tokio::test]
async fn mcp_auth_uses_existing_credentials_and_escapes_server_and_location() -> TestResult {
    let (response, captured) = exchange(
        "docs/日本",
        McpAuthRequestDto::Logout {},
        200,
        br#"{"login_id":null,"state":"signed_out","authorization_url":null}"#.to_vec(),
    )
    .await?;
    assert_eq!(response?.state, McpAuthState::SignedOut);
    let lower = captured.to_ascii_lowercase();
    assert!(lower.starts_with("post /api/mcp/%64%6f%63%73%2f%e6%97%a5%e6%9c%ac/auth?location%5bdirectory%5d=%2frepo+with+space http/1.1"), "{captured}");
    assert!(lower.contains("authorization: basic dgvzddp0zxn0"));
    assert!(captured.ends_with(r#"{"action":"logout"}"#));
    Ok(())
}

#[tokio::test]
async fn mcp_auth_rejects_a_foreign_attempt_redirect_and_oversized_response() -> TestResult {
    let request: McpAuthRequestDto =
        serde_json::from_str(r#"{"action":"status","login_id":"mcl_expected"}"#)?;
    let (result, _) = exchange(
        "docs",
        request,
        200,
        br#"{"login_id":"mcl_other","state":"signed_in","authorization_url":null}"#.to_vec(),
    )
    .await?;
    assert!(matches!(result, Err(ApiClientError::Decode)));
    let (result, _) = exchange("docs", McpAuthRequestDto::Logout {}, 307, Vec::new()).await?;
    assert!(matches!(result, Err(ApiClientError::Status(code)) if code.as_u16() == 307));
    let (result, _) =
        exchange("docs", McpAuthRequestDto::Logout {}, 200, vec![b' '; 65537]).await?;
    assert!(matches!(
        result,
        Err(ApiClientError::ResponseTooLarge { maximum: 65536 })
    ));
    Ok(())
}
