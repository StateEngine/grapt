//! Ordinary authenticated HTTP host for Session configuration witnesses.
use crate::dogfood::ServerApplication;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn host(
    application: &Arc<ServerApplication>,
) -> Result<grapt_api::RunningApiHost, Box<dyn std::error::Error>> {
    let limits = crate::compose_host::host_limits()?;
    let config = crate::input::ServiceConfig {
        slot: "product".into(),
        instance: "coder".into(),
        environment: "acceptance".into(),
        build: "seat03-fixture".into(),
        version: "0.1.0".into(),
        serve_window_ms: 10000,
        provider_connect_ms: 1000,
        provider_response_head_ms: 1000,
        provider_active_attempts: 1,
        close_grace_ms: None,
        max_inflight_streams: None,
    };
    let composition = crate::compose::ServerComposition::ready(
        application.clone(),
        application.clone(),
        application.hub.clone(),
        None,
    );
    let service = crate::compose_host::product_service(
        &limits,
        &crate::input::Credential {
            username: "fixture".into(),
            secret: "secret".into(),
        },
        grapt_types::service::ProcessId::new(std::process::id()).ok_or("process")?,
        &config,
        Some(composition),
    )?;
    Ok(grapt_api::PreparedApiHost::new(
        crate::compose_host::bind_loopback().await?,
        service,
        limits,
        Arc::new(grapt_api::TokioDeadlineDriver::new()),
    )?
    .start())
}

pub(super) async fn call(
    application: &Arc<ServerApplication>,
    method: &str,
    path: &str,
    body: &str,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let host = host(application).await?;
    let address = host.ready().observed_address();
    let result = async {
        let mut socket = TcpStream::connect(address).await?;
        let request = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic Zml4dHVyZTpzZWNyZXQ=\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        socket.write_all(request.as_bytes()).await?;
        let mut response = Vec::new();
        loop {
            let mut chunk = [0; 2048];
            let count = tokio::time::timeout(std::time::Duration::from_secs(5), socket.read(&mut chunk)).await??;
            if count == 0 { return Ok::<_, Box<dyn std::error::Error>>(response); }
            if response.len() + count > grapt_api::u3a::U3A_PRODUCTION_LIMITS.page_bytes + 8192 {
                return Err("fixture HTTP response exceeded its page and header bound".into());
            }
            response.extend_from_slice(&chunk[..count]);
        }
    }.await;
    assert!(
        matches!(
            host.stop().await,
            grapt_api::ApiHostExit::StoppedGracefully { .. }
        ),
        "fixture API host did not join cleanly"
    );
    result
}

pub(super) fn page<T: serde::de::DeserializeOwned>(
    response: &[u8],
) -> Result<T, Box<dyn std::error::Error>> {
    assert!(
        response.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(response)
    );
    let end = response
        .windows(4)
        .position(|part| part == b"\r\n\r\n")
        .ok_or("HTTP head")?;
    Ok(grapt_api::u3a::decode_page(
        &response[end + 4..],
        grapt_api::u3a::U3aLimits::production().map_err(|error| format!("{error:?}"))?,
    )
    .map_err(|error| {
        format!(
            "{} decode failed: {error:?}; received {} bytes; HTTP head: {}",
            std::any::type_name::<T>(),
            response.len(),
            String::from_utf8_lossy(&response[..end]),
        )
    })?)
}

pub(super) async fn approver(
    application: &Arc<ServerApplication>,
    query: &str,
) -> Result<(grapt_api::RunningApiHost, TcpStream), Box<dyn std::error::Error>> {
    let host = host(application).await?;
    let mut stream = TcpStream::connect(host.ready().observed_address()).await?;
    stream.write_all(format!("GET /api/event?{query} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic Zml4dHVyZTpzZWNyZXQ=\r\nx-grapt-approver: *\r\n\r\n").as_bytes()).await?;
    let mut head = Vec::new();
    for _ in 0..8192 {
        head.push(tokio::time::timeout(Duration::from_secs(5), stream.read_u8()).await??);
        if head.ends_with(b"\r\n\r\n") {
            assert!(
                head.starts_with(b"HTTP/1.1 200"),
                "{}",
                String::from_utf8_lossy(&head)
            );
            return Ok((host, stream));
        }
    }
    Err("approver response exceeded its header bound".into())
}

type PeerResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// The bounded HTTP framing used by the existing MCP remote test peer.
async fn peer_request(stream: &mut TcpStream) -> PeerResult<(bool, Value)> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let count = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await??;
        if count == 0 || bytes.len() + count > 8192 {
            return Err("fixture request missing or oversized".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let head = std::str::from_utf8(&bytes[..end])?;
            let length = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>())
                })
                .transpose()?
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                let body = &bytes[end + 4..end + 4 + length];
                return Ok((
                    head.starts_with("GET "),
                    if body.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_slice(body)?
                    },
                ));
            }
        }
    }
}

async fn peer_reply(stream: &mut TcpStream, content_type: &str, body: &str) -> PeerResult {
    stream.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await?;
    Ok(())
}

pub(super) struct ContentPeer {
    pub url: String,
    task: tokio::task::JoinHandle<PeerResult>,
}
impl Drop for ContentPeer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl ContentPeer {
    pub async fn finish(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        tokio::time::timeout(Duration::from_secs(5), &mut self.task)
            .await??
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

pub(super) async fn content_peer(
    label: &'static str,
    consent: bool,
) -> Result<ContentPeer, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}/mcp", listener.local_addr()?);
    let task = tokio::spawn(async move {
        for _ in 0..32 {
            let (mut stream, _) =
                tokio::time::timeout(Duration::from_secs(5), listener.accept()).await??;
            let (get, message) = peer_request(&mut stream).await?;
            if get || message["method"] == "notifications/initialized" {
                let status = if get {
                    "405 Method Not Allowed"
                } else {
                    "202 Accepted"
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await?;
                continue;
            }
            let method = message["method"].as_str().ok_or("fixture method")?;
            let result = match method {
                "initialize" => {
                    json!({"protocolVersion":"2025-11-25","serverInfo":{"name":label,"version":"1"},"capabilities":{"tools":{},"prompts":{"listChanged":true},"resources":{"listChanged":true}}})
                }
                "tools/list" => json!({"tools":[]}),
                "prompts/list" => {
                    json!({"prompts":[{"name":format!("{label}_prompt"),"arguments":[]}]})
                }
                "resources/list" => {
                    json!({"resources":[{"name":format!("{label}_resource"),"uri":"fixture://content","mimeType":"text/plain"}]})
                }
                "resources/templates/list" => json!({"resourceTemplates":[]}),
                "resources/read" => {
                    assert_eq!(message["params"]["uri"], "fixture://content");
                    json!({"contents":[{"uri":"fixture://content","mimeType":"text/plain","text":format!("{label} private content")}]})
                }
                _ => return Err("unexpected content fixture method".into()),
            };
            let reply = json!({"jsonrpc":"2.0","id":message["id"],"result":result});
            if method == "initialize" && consent {
                let ask = json!({"jsonrpc":"2.0","id":"startup-consent","method":"elicitation/create","params":{"message":"Alice startup","requestedSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}}});
                let first = format!("data: {ask}\n\n");
                let last = format!("data: {reply}\n\n");
                stream.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{first}", first.len() + last.len()).as_bytes()).await?;
                let (mut answer, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept()).await??;
                let (_, answer_message) = peer_request(&mut answer).await?;
                assert_eq!(answer_message["id"], "startup-consent");
                assert_eq!(
                    answer_message["result"],
                    json!({"action":"accept","content":{"name":"Alice"}})
                );
                answer
                    .write_all(
                        b"HTTP/1.1 202 Accepted\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                    )
                    .await?;
                stream.write_all(last.as_bytes()).await?;
            } else if method == "resources/read" {
                let changed =
                    json!({"jsonrpc":"2.0","method":"notifications/resources/list_changed"});
                peer_reply(
                    &mut stream,
                    "text/event-stream",
                    &format!("data: {changed}\n\ndata: {reply}\n\n"),
                )
                .await?;
                return Ok(());
            } else {
                peer_reply(&mut stream, "application/json", &reply.to_string()).await?;
            }
        }
        Err("content fixture exceeded its request bound".into())
    });
    Ok(ContentPeer { url, task })
}
