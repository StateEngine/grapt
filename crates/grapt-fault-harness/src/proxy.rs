//! In-process deterministic TCP/HTTP/SSE chaos proxy.
//!
//! All fault triggers are driven by deterministic request, frame, or byte counts
//! via [`FaultSchedule`] rather than wall-clock sleeps.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Notify};
use tokio::task::JoinHandle;

use crate::schedule::{FailureTranscript, FaultAction, FaultSchedule};

const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// Errors produced while binding or running [`ChaosProxy`].
#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("failed to bind chaos proxy listener: {0}")]
    Bind(#[source] std::io::Error),
    #[error("failed to read local listener address: {0}")]
    LocalAddr(#[source] std::io::Error),
}

/// Configuration and constructor for an in-process HTTP/SSE chaos proxy.
#[derive(Debug, Clone)]
pub struct ChaosProxy {
    upstream: Option<SocketAddr>,
    schedule: FaultSchedule,
}

impl ChaosProxy {
    /// Create a proxy that forwards to `upstream` under `schedule`.
    #[must_use]
    pub fn new(upstream: SocketAddr, schedule: FaultSchedule) -> Self {
        Self {
            upstream: Some(upstream),
            schedule,
        }
    }

    /// Create a proxy with no live upstream (for simulating a down service).
    #[must_use]
    pub fn disconnected(schedule: FaultSchedule) -> Self {
        Self {
            upstream: None,
            schedule,
        }
    }

    /// Bind on `127.0.0.1:0` and start serving connections on the current Tokio runtime.
    ///
    /// # Errors
    /// Returns [`ProxyError`] if the loopback listener cannot bind.
    pub async fn start(self) -> Result<ChaosProxyHandle, ProxyError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(ProxyError::Bind)?;
        let addr = listener.local_addr().map_err(ProxyError::LocalAddr)?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let delay_gate = Arc::new(Notify::new());
        let schedule = self.schedule.clone();
        let upstream = self.upstream;
        let gate_for_task = Arc::clone(&delay_gate);

        let task = tokio::spawn(async move {
            run_accept_loop(listener, upstream, schedule, gate_for_task, shutdown_rx).await;
        });

        Ok(ChaosProxyHandle {
            addr,
            schedule: self.schedule,
            delay_gate,
            shutdown_tx: Some(shutdown_tx),
            task: Some(task),
        })
    }
}

/// Running handle to a [`ChaosProxy`].
pub struct ChaosProxyHandle {
    addr: SocketAddr,
    schedule: FaultSchedule,
    delay_gate: Arc<Notify>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl fmt::Debug for ChaosProxyHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChaosProxyHandle")
            .field("addr", &self.addr)
            .field("seed", &self.schedule.seed())
            .finish()
    }
}

impl ChaosProxyHandle {
    /// Return the loopback socket address the proxy is listening on.
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Release any connections waiting on a deterministic `DelayChunk` gate.
    pub fn release_delayed(&self) {
        self.delay_gate.notify_waiters();
    }

    /// Snapshot the current [`FailureTranscript`].
    #[must_use]
    pub fn transcript(&self) -> FailureTranscript {
        self.schedule.transcript()
    }

    /// Stop accepting new connections and wait for the accept loop to exit.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.delay_gate.notify_waiters();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for ChaosProxyHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.delay_gate.notify_waiters();
    }
}

async fn run_accept_loop(
    listener: TcpListener,
    upstream: Option<SocketAddr>,
    schedule: FaultSchedule,
    delay_gate: Arc<Notify>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accepted = listener.accept() => {
                let Ok((client, _peer)) = accepted else {
                    break;
                };
                let sched = schedule.clone();
                let gate = Arc::clone(&delay_gate);
                tokio::spawn(async move {
                    handle_connection(client, upstream, sched, gate).await;
                });
            }
        }
    }
}

async fn handle_connection(
    mut client: TcpStream,
    upstream: Option<SocketAddr>,
    schedule: FaultSchedule,
    delay_gate: Arc<Notify>,
) {
    let Some(raw_request) = read_http_request(&mut client).await else {
        return;
    };
    let (method, path) = parse_request_line(&raw_request);
    let action = schedule.next_request_action(&method, &path);

    match action {
        FaultAction::InjectHttpStatus {
            status,
            retry_after_secs,
            body,
        } => {
            let response = build_synthetic_http_response(status, retry_after_secs, &body);
            let _ = client.write_all(&response).await;
            let _ = client.shutdown().await;
        }
        FaultAction::UpstreamDown { status, reason } => {
            let response = build_synthetic_http_response(status, None, &reason);
            let _ = client.write_all(&response).await;
            let _ = client.shutdown().await;
        }
        other => {
            let Some(upstream_addr) = upstream else {
                let response =
                    build_synthetic_http_response(503, None, "upstream unavailable in chaos proxy");
                let _ = client.write_all(&response).await;
                let _ = client.shutdown().await;
                return;
            };
            let Some(upstream_bytes) = fetch_upstream(upstream_addr, &raw_request).await else {
                let response =
                    build_synthetic_http_response(502, None, "upstream connection failed");
                let _ = client.write_all(&response).await;
                let _ = client.shutdown().await;
                return;
            };
            deliver_with_fault(&mut client, &upstream_bytes, other, &delay_gate).await;
        }
    }
}

async fn deliver_with_fault(
    client: &mut TcpStream,
    upstream_bytes: &[u8],
    action: FaultAction,
    delay_gate: &Notify,
) {
    let body_offset = find_header_end(upstream_bytes).unwrap_or(0);
    match action {
        FaultAction::PassThrough => {
            let _ = client.write_all(upstream_bytes).await;
            let _ = client.shutdown().await;
        }
        FaultAction::MidStreamEofBytes { after_bytes }
        | FaultAction::TruncateBody {
            keep_bytes: after_bytes,
        } => {
            let cut = body_offset
                .saturating_add(after_bytes)
                .min(upstream_bytes.len());
            let _ = client.write_all(&upstream_bytes[..cut]).await;
            let _ = client.shutdown().await;
        }
        FaultAction::MidStreamEofFrames { after_frames } => {
            let cut = find_sse_frame_boundary(upstream_bytes, body_offset, after_frames);
            let _ = client.write_all(&upstream_bytes[..cut]).await;
            let _ = client.shutdown().await;
        }
        FaultAction::MidStreamRst { after_bytes } => {
            let cut = body_offset
                .saturating_add(after_bytes)
                .min(upstream_bytes.len());
            let _ = client.write_all(&upstream_bytes[..cut]).await;
            // Drop without graceful shutdown to simulate an abrupt mid-stream reset.
        }
        FaultAction::DelayChunk {
            after_bytes,
            yield_steps,
            wait_for_gate,
        } => {
            let split = body_offset
                .saturating_add(after_bytes)
                .min(upstream_bytes.len());
            let _ = client.write_all(&upstream_bytes[..split]).await;
            let _ = client.flush().await;
            for _ in 0..yield_steps {
                tokio::task::yield_now().await;
            }
            if wait_for_gate {
                delay_gate.notified().await;
            }
            let _ = client.write_all(&upstream_bytes[split..]).await;
            let _ = client.shutdown().await;
        }
        FaultAction::InjectHttpStatus { .. } | FaultAction::UpstreamDown { .. } => {}
    }
}

async fn read_http_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() >= MAX_REQUEST_BYTES {
            return None;
        }
    };

    let content_length = parse_content_length(&buf[..header_end]);
    let target_len = header_end.saturating_add(content_length);
    while buf.len() < target_len && buf.len() < MAX_REQUEST_BYTES {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(buf)
}

async fn fetch_upstream(addr: SocketAddr, request: &[u8]) -> Option<Vec<u8>> {
    let mut upstream = TcpStream::connect(addr).await.ok()?;
    upstream.write_all(request).await.ok()?;
    let _ = upstream.shutdown().await;

    let mut response = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    loop {
        let n = upstream.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        let remaining = MAX_RESPONSE_BYTES.saturating_sub(response.len());
        if remaining == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..n.min(remaining)]);
    }
    Some(response)
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

fn find_sse_frame_boundary(buf: &[u8], body_offset: usize, frames: usize) -> usize {
    if frames == 0 {
        return body_offset.min(buf.len());
    }
    let body = &buf[body_offset.min(buf.len())..];
    let mut count = 0;
    let mut idx = 0;
    while idx + 1 < body.len() {
        if body[idx] == b'\n' && body[idx + 1] == b'\n' {
            count += 1;
            idx += 2;
            if count == frames {
                return body_offset.saturating_add(idx);
            }
        } else {
            idx += 1;
        }
    }
    buf.len()
}

fn parse_request_line(buf: &[u8]) -> (String, String) {
    let header_text = String::from_utf8_lossy(buf);
    let first_line = header_text.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_owned();
    let path = parts.next().unwrap_or("/").to_owned();
    (method, path)
}

fn parse_content_length(headers: &[u8]) -> usize {
    let text = String::from_utf8_lossy(headers);
    for line in text.lines() {
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                if let Ok(len) = value.trim().parse::<usize>() {
                    return len;
                }
            }
        }
    }
    0
}

fn build_synthetic_http_response(
    status: u16,
    retry_after_secs: Option<u64>,
    body: &str,
) -> Vec<u8> {
    let reason = match status {
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        529 => "Site Is Overloaded",
        _ => "Chaos Injected Error",
    };
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(secs) = retry_after_secs {
        out.push_str(&format!("Retry-After: {secs}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    out.into_bytes()
}
