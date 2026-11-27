use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::TestResult;

#[derive(Debug)]
pub(super) struct Script {
    method: String,
    path: String,
    status: u16,
    content_type: &'static str,
    body: String,
    expected_json: Option<serde_json::Value>,
    open_sse_chunks: Option<Vec<String>>,
    deferred_sse: Option<(std::sync::Arc<tokio::sync::Notify>, String)>,
    require_live_sse: bool,
}

impl Script {
    pub(super) fn status(method: impl Into<String>, path: impl Into<String>, status: u16) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            status,
            content_type: "text/plain",
            body: String::new(),
            expected_json: None,
            open_sse_chunks: None,
            deferred_sse: None,
            require_live_sse: false,
        }
    }

    pub(super) fn json(
        method: impl Into<String>,
        path: impl Into<String>,
        status: u16,
        body: &str,
    ) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            status,
            content_type: "application/json",
            body: body.to_owned(),
            expected_json: None,
            open_sse_chunks: None,
            deferred_sse: None,
            require_live_sse: false,
        }
    }

    pub(super) fn sse(method: impl Into<String>, path: impl Into<String>, body: &str) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            status: 200,
            content_type: "text/event-stream",
            body: body.to_owned(),
            expected_json: None,
            open_sse_chunks: None,
            deferred_sse: None,
            require_live_sse: false,
        }
    }

    /// Separate HTTP chunks followed by an open stream: a pending-frame
    /// overflow cannot borrow EOF as its recovery trigger.
    pub(super) fn sse_chunks_open(path: impl Into<String>, chunks: Vec<String>) -> Self {
        let mut script = Self::sse("GET", path, "");
        script.open_sse_chunks = Some(chunks);
        script
    }

    /// Release one bounded flood only after the host observes the valid prefix.
    pub(super) fn defer_sse(
        mut self,
        signal: std::sync::Arc<tokio::sync::Notify>,
        body: String,
    ) -> Self {
        self.deferred_sse = Some((signal, body));
        self
    }

    pub(super) fn expect_json(mut self, value: serde_json::Value) -> Self {
        self.expected_json = Some(value);
        self
    }

    pub(super) fn require_live_sse(mut self) -> Self {
        self.require_live_sse = true;
        self
    }
}

pub(super) async fn spawn_server(
    scripts: Vec<Script>,
) -> TestResult<(String, tokio::task::JoinHandle<TestResult<Vec<String>>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let mut remaining = scripts;
        let mut requests = Vec::new();
        let mut held_streams: Vec<tokio::net::TcpStream> = Vec::new();
        let mut deferred: Option<(usize, std::sync::Arc<tokio::sync::Notify>, String)> = None;
        while !remaining.is_empty() {
            let (mut stream, _) = if let Some((index, signal, body)) = deferred.take() {
                tokio::select! {
                    accepted = listener.accept() => {
                        deferred = Some((index, signal.clone(), body));
                        accepted?
                    }
                    _ = signal.notified() => {
                        held_streams[index].write_all(
                            format!("{:x}\r\n{}\r\n", body.len(), body).as_bytes()
                        ).await?;
                        listener.accept().await?
                    }
                }
            } else {
                listener.accept().await?
            };
            let request = read_request(&mut stream).await?;
            let first = request.lines().next().ok_or("request line absent")?;
            let mut words = first.split_whitespace();
            let method = words.next().ok_or("request method absent")?;
            let path = words.next().ok_or("request path absent")?;
            let position = remaining
                .iter()
                .position(|script| method == script.method && path == script.path)
                .ok_or_else(|| format!("no scripted response for {method} {path}"))?;
            let script = remaining.remove(position);
            if script.require_live_sse {
                let approver = held_streams.first().ok_or("approver stream absent")?;
                let mut byte = [0];
                if tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    approver.peek(&mut byte),
                )
                .await
                .is_ok()
                {
                    return Err("approver stream closed before later permission".into());
                }
            }
            if let Some(expected) = &script.expected_json {
                let (_, body) = request
                    .split_once("\r\n\r\n")
                    .ok_or("request body delimiter absent")?;
                let actual: serde_json::Value = serde_json::from_str(body)?;
                if actual != *expected {
                    return Err(
                        format!("unexpected JSON body for {method} {path}: {actual}").into(),
                    );
                }
            }
            requests.push(format!("{method} {path}"));
            write_response(&mut stream, &script).await?;
            if script.open_sse_chunks.is_some() {
                if let Some((signal, body)) = script.deferred_sse {
                    if deferred.is_some() {
                        return Err("fixture supports only one deferred flood".into());
                    }
                    deferred = Some((held_streams.len(), signal, body));
                }
                held_streams.push(stream);
            }
        }
        drop(held_streams);
        Ok(requests)
    });
    Ok((format!("http://{address}"), server))
}

pub(super) async fn spawn_retrying_catalog_server(
    cwd: &str,
) -> TestResult<(
    String,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<TestResult<Vec<String>>>,
)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (stop, mut stopped) = oneshot::channel();
    let encoded = encode_query_component(cwd);
    let query = format!("?location%5Bdirectory%5D={encoded}");
    let default_path = format!("/api/model/default{query}");
    let models_path = format!("/api/model{query}");
    let agents_path = format!("/api/agent{query}");
    let commands_path = format!("/api/command{query}");
    let skills_path = format!("/api/skill{query}");
    let session = format!(
        r#"{{"data":{{"id":"ses_retry","title":"ACP","location":{{"directory":"{cwd}"}},"agent":"build","model":{{"providerID":"vendor","id":"model","variant":"high"}}}}}}"#
    );
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        let mut default_attempts = 0usize;
        loop {
            let accepted = tokio::select! {
                _ = &mut stopped => break,
                accepted = listener.accept() => accepted,
            };
            let (mut stream, _) = accepted?;
            let request = read_request(&mut stream).await?;
            let first = request.lines().next().ok_or("request line absent")?;
            let mut words = first.split_whitespace();
            let method = words.next().ok_or("request method absent")?;
            let path = words.next().ok_or("request path absent")?;
            let script = if method == "GET" && path == default_path {
                default_attempts = default_attempts.saturating_add(1);
                if default_attempts == 1 {
                    Script::json("GET", path, 500, r#"{"error":"catalog composing"}"#)
                } else {
                    Script::json(
                        "GET",
                        path,
                        200,
                        r#"{"data":{"providerID":"vendor","id":"model","variant":"high"}}"#,
                    )
                }
            } else if method == "GET" && path == models_path {
                Script::json(
                    "GET",
                    path,
                    200,
                    r#"{"data":[{"providerID":"vendor","id":"model","name":"Model","enabled":true,"variants":[{"id":"high"}]}]}"#,
                )
            } else if method == "GET" && path == agents_path {
                Script::json(
                    "GET",
                    path,
                    200,
                    r#"{"data":[{"id":"build","name":"Build","mode":"primary","hidden":false}]}"#,
                )
            } else if method == "GET" && (path == commands_path || path == skills_path) {
                Script::json("GET", path, 200, r#"{"data":[]}"#)
            } else if method == "POST" && path == "/api/session" {
                Script::json("POST", path, 200, &session)
            } else {
                return Err(format!("unexpected retry fixture request: {method} {path}").into());
            };
            requests.push(format!("{method} {path}"));
            write_response(&mut stream, &script).await?;
        }
        Ok(requests)
    });
    Ok((format!("http://{address}"), stop, server))
}

fn encode_query_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

async fn write_response(stream: &mut tokio::net::TcpStream, script: &Script) -> TestResult {
    if let Some(chunks) = &script.open_sse_chunks {
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await?;
        for chunk in chunks {
            stream
                .write_all(format!("{:x}\r\n{}\r\n", chunk.len(), chunk).as_bytes())
                .await?;
        }
        return Ok(());
    }
    let reason = match script.status {
        200 => "OK",
        409 => "Conflict",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let response = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        script.status,
        script.content_type,
        script.body.len(),
        script.body
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

pub(super) async fn read_request(stream: &mut tokio::net::TcpStream) -> TestResult<String> {
    const MAX_REQUEST: usize = 65_536;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4_096];
    loop {
        let amount = stream.read(&mut chunk).await?;
        if amount == 0 {
            break;
        }
        if bytes.len().saturating_add(amount) > MAX_REQUEST {
            return Err("request exceeded test server bound".into());
        }
        bytes.extend_from_slice(&chunk[..amount]);
        if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = core::str::from_utf8(&bytes[..header_end + 4])?;
            let content_length = head
                .lines()
                .find_map(|line| {
                    line.strip_prefix("content-length: ")
                        .or_else(|| line.strip_prefix("Content-Length: "))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                break;
            }
        }
    }
    Ok(String::from_utf8(bytes)?)
}
