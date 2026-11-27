//! In-process deterministic TCP/HTTP/SSE chaos proxy.
//!
//! Streams upstream responses live chunk-by-chunk, bounds header buffering,
//! decodes HTTP `Transfer-Encoding: chunked` framing when evaluating byte and
//! LF/CRLF SSE frame faults, sends real `SO_LINGER(0)` TCP resets, uses a
//! stateful semaphore gate with zero wall-clock sleeps, and aborts/joins all
//! active connection handlers before [`ChaosProxyHandle::shutdown`] returns.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, Semaphore};
use tokio::task::{JoinHandle, JoinSet};

use crate::framing::{
    build_synthetic_http_response, encode_http_chunk, find_header_end,
    is_chunked_transfer_encoding, parse_content_length, parse_request_line, ChunkedDecoder,
    SseBoundary, SseFrameScanner, MAX_RESPONSE_HEADER_BYTES,
};
use crate::schedule::{FailureTranscript, FaultAction, FaultSchedule};

const MAX_REQUEST_BYTES: usize = 256 * 1024;
const STREAM_CHUNK_BYTES: usize = 4096;

/// Errors produced while binding or running [`ChaosProxy`].
#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("failed to bind chaos proxy listener: {0}")]
    Bind(#[source] std::io::Error),
    #[error("failed to read local listener address: {0}")]
    LocalAddr(#[source] std::io::Error),
}

/// Stateful deterministic gate for [`FaultAction::DelayChunk`].
///
/// Uses semaphore permits so releases issued before the proxy reaches the gate
/// are never lost and callers can deterministically await gate entry without
/// guessing yield counts.
#[derive(Debug)]
struct DelayGate {
    entered: Semaphore,
    release: Semaphore,
}

impl DelayGate {
    fn new() -> Self {
        Self {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        }
    }

    async fn enter_and_wait(&self) {
        self.entered.add_permits(1);
        if let Ok(permit) = self.release.acquire().await {
            permit.forget();
        }
    }

    async fn wait_until_entered(&self) {
        if let Ok(permit) = self.entered.acquire().await {
            permit.forget();
        }
    }

    fn release_one(&self) {
        self.release.add_permits(1);
    }

    fn close(&self) {
        self.entered.close();
        self.release.close();
    }
}

/// Configuration and constructor for an in-process HTTP/SSE chaos proxy.
#[derive(Debug, Clone)]
pub struct ChaosProxy {
    upstream: Option<SocketAddr>,
    schedule: FaultSchedule,
}

impl ChaosProxy {
    /// Create a proxy that streams traffic to `upstream` under `schedule`.
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
        let delay_gate = Arc::new(DelayGate::new());
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
    delay_gate: Arc<DelayGate>,
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

    /// Await until one connection has flushed its pre-delay chunk and entered the gate.
    pub async fn wait_until_delayed(&self) {
        self.delay_gate.wait_until_entered().await;
    }

    /// Release one delayed connection (stateful: safe to call before or after gate entry).
    pub fn release_delayed(&self) {
        self.delay_gate.release_one();
    }

    /// Snapshot the current [`FailureTranscript`].
    #[must_use]
    pub fn transcript(&self) -> FailureTranscript {
        self.schedule.transcript()
    }

    /// Stop accepting new connections and abort/join all active connection handlers.
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        self.delay_gate.close();
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
        self.delay_gate.close();
    }
}

async fn run_accept_loop(
    listener: TcpListener,
    upstream: Option<SocketAddr>,
    schedule: FaultSchedule,
    delay_gate: Arc<DelayGate>,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let mut handlers: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => break,
            accepted = listener.accept() => {
                let Ok((client, _peer)) = accepted else {
                    break;
                };
                let sched = schedule.clone();
                let gate = Arc::clone(&delay_gate);
                handlers.spawn(async move {
                    handle_connection(client, upstream, sched, gate).await;
                });
            }
            Some(_) = handlers.join_next(), if !handlers.is_empty() => {}
        }
    }
    handlers.abort_all();
    while handlers.join_next().await.is_some() {}
}

async fn handle_connection(
    mut client: TcpStream,
    upstream: Option<SocketAddr>,
    schedule: FaultSchedule,
    delay_gate: Arc<DelayGate>,
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
            stream_from_upstream(client, upstream_addr, &raw_request, other, &delay_gate).await;
        }
    }
}

async fn stream_from_upstream(
    mut client: TcpStream,
    upstream_addr: SocketAddr,
    raw_request: &[u8],
    action: FaultAction,
    delay_gate: &DelayGate,
) {
    let Ok(mut upstream) = TcpStream::connect(upstream_addr).await else {
        send_error_and_close(&mut client, 502, "upstream connection failed").await;
        return;
    };
    if upstream.write_all(raw_request).await.is_err() {
        send_error_and_close(&mut client, 502, "upstream write failed").await;
        return;
    }
    let _ = upstream.shutdown().await;

    let mut state = StreamState::new(action);
    let mut header_buf = Vec::with_capacity(512);
    let mut chunk = [0_u8; STREAM_CHUNK_BYTES];

    loop {
        let Ok(n) = upstream.read(&mut chunk).await else {
            let _ = client.shutdown().await;
            return;
        };
        if n == 0 {
            if !state.headers_sent && !header_buf.is_empty() {
                let _ = client.write_all(&header_buf).await;
            }
            let _ = client.shutdown().await;
            return;
        }

        if !state.headers_sent {
            header_buf.extend_from_slice(&chunk[..n]);
            let Some(end) = find_header_end(&header_buf) else {
                if header_buf.len() > MAX_RESPONSE_HEADER_BYTES {
                    send_error_and_close(
                        &mut client,
                        502,
                        "upstream response headers exceeded limit",
                    )
                    .await;
                    return;
                }
                continue;
            };
            if end > MAX_RESPONSE_HEADER_BYTES {
                send_error_and_close(&mut client, 502, "upstream response headers exceeded limit")
                    .await;
                return;
            }
            state.configure_body_mode(&header_buf[..end]);
            if client.write_all(&header_buf[..end]).await.is_err() || client.flush().await.is_err()
            {
                return;
            }
            state.headers_sent = true;
            let trailing = header_buf.split_off(end);
            if !trailing.is_empty() {
                let decision = state
                    .forward_wire_body(&mut client, &trailing, delay_gate)
                    .await;
                if !apply_decision(&mut client, decision).await {
                    return;
                }
            }
            continue;
        }

        let decision = state
            .forward_wire_body(&mut client, &chunk[..n], delay_gate)
            .await;
        if !apply_decision(&mut client, decision).await {
            return;
        }
    }
}

async fn send_error_and_close(client: &mut TcpStream, status: u16, reason: &str) {
    let response = build_synthetic_http_response(status, None, reason);
    let _ = client.write_all(&response).await;
    let _ = client.shutdown().await;
}

async fn apply_decision(client: &mut TcpStream, decision: StreamDecision) -> bool {
    match decision {
        StreamDecision::Continue => true,
        StreamDecision::CloseEof => {
            let _ = client.shutdown().await;
            false
        }
        StreamDecision::AbortRst => {
            let _ = client.set_zero_linger();
            false
        }
    }
}

enum StreamDecision {
    Continue,
    CloseEof,
    AbortRst,
}

struct StreamState {
    action: FaultAction,
    headers_sent: bool,
    chunked_decoder: Option<ChunkedDecoder>,
    sse_scanner: SseFrameScanner,
    body_bytes_sent: usize,
    frames_seen: usize,
    delay_triggered: bool,
}

impl StreamState {
    fn new(action: FaultAction) -> Self {
        Self {
            action,
            headers_sent: false,
            chunked_decoder: None,
            sse_scanner: SseFrameScanner::default(),
            body_bytes_sent: 0,
            frames_seen: 0,
            delay_triggered: false,
        }
    }

    fn configure_body_mode(&mut self, headers: &[u8]) {
        if is_chunked_transfer_encoding(headers) {
            self.chunked_decoder = Some(ChunkedDecoder::new());
        }
    }

    async fn forward_wire_body(
        &mut self,
        client: &mut TcpStream,
        wire: &[u8],
        delay_gate: &DelayGate,
    ) -> StreamDecision {
        // Relay PassThrough responses verbatim so chunk framing and trailers stay intact.
        if matches!(self.action, FaultAction::PassThrough) {
            return write_and_flush(client, wire).await;
        }
        if let Some(decoder) = &mut self.chunked_decoder {
            let decoded = decoder.decode(wire);
            if decoded.malformed {
                return StreamDecision::CloseEof;
            }
            if !decoded.payload.is_empty() {
                let decision = self
                    .forward_logical_body(client, &decoded.payload, delay_gate, true)
                    .await;
                if !matches!(decision, StreamDecision::Continue) {
                    return decision;
                }
            }
            if decoded.finished {
                let _ = write_and_flush(client, b"0\r\n\r\n").await;
                return StreamDecision::CloseEof;
            }
            StreamDecision::Continue
        } else {
            self.forward_logical_body(client, wire, delay_gate, false)
                .await
        }
    }

    async fn forward_logical_body(
        &mut self,
        client: &mut TcpStream,
        slice: &[u8],
        delay_gate: &DelayGate,
        chunked: bool,
    ) -> StreamDecision {
        match &self.action {
            FaultAction::PassThrough => write_logical(client, slice, chunked).await,
            FaultAction::MidStreamEofBytes { after_bytes }
            | FaultAction::TruncateBody {
                keep_bytes: after_bytes,
            } => {
                self.forward_capped(client, slice, *after_bytes, false, chunked)
                    .await
            }
            FaultAction::MidStreamRst { after_bytes } => {
                self.forward_capped(client, slice, *after_bytes, true, chunked)
                    .await
            }
            FaultAction::MidStreamEofFrames { after_frames } => {
                self.forward_until_frames(client, slice, *after_frames, chunked)
                    .await
            }
            FaultAction::DelayChunk {
                after_bytes,
                yield_steps,
                wait_for_gate,
            } => {
                self.forward_delayed(
                    client,
                    slice,
                    DelaySpec {
                        after_bytes: *after_bytes,
                        yield_steps: *yield_steps,
                        wait_for_gate: *wait_for_gate,
                        chunked,
                    },
                    delay_gate,
                )
                .await
            }
            FaultAction::InjectHttpStatus { .. } | FaultAction::UpstreamDown { .. } => {
                StreamDecision::CloseEof
            }
        }
    }

    async fn forward_capped(
        &mut self,
        client: &mut TcpStream,
        slice: &[u8],
        limit: usize,
        reset: bool,
        chunked: bool,
    ) -> StreamDecision {
        let remaining = limit.saturating_sub(self.body_bytes_sent);
        let take = remaining.min(slice.len());
        if take > 0
            && !matches!(
                write_logical(client, &slice[..take], chunked).await,
                StreamDecision::Continue
            )
        {
            return StreamDecision::CloseEof;
        }
        self.body_bytes_sent = self.body_bytes_sent.saturating_add(take);
        if self.body_bytes_sent >= limit {
            if reset {
                StreamDecision::AbortRst
            } else {
                StreamDecision::CloseEof
            }
        } else {
            StreamDecision::Continue
        }
    }

    async fn forward_until_frames(
        &mut self,
        client: &mut TcpStream,
        slice: &[u8],
        target_frames: usize,
        chunked: bool,
    ) -> StreamDecision {
        if target_frames == 0 {
            return StreamDecision::CloseEof;
        }
        for (idx, &byte) in slice.iter().enumerate() {
            match self.sse_scanner.push_byte(byte) {
                SseBoundary::None => {}
                SseBoundary::CompleteAtCurrent => {
                    self.frames_seen = self.frames_seen.saturating_add(1);
                    if self.frames_seen >= target_frames {
                        let _ = write_logical(client, &slice[..=idx], chunked).await;
                        self.body_bytes_sent = self.body_bytes_sent.saturating_add(idx + 1);
                        return StreamDecision::CloseEof;
                    }
                }
                SseBoundary::CompleteBeforeCurrent => {
                    self.frames_seen = self.frames_seen.saturating_add(1);
                    if self.frames_seen >= target_frames {
                        let _ = write_logical(client, &slice[..idx], chunked).await;
                        self.body_bytes_sent = self.body_bytes_sent.saturating_add(idx);
                        return StreamDecision::CloseEof;
                    }
                }
            }
        }
        self.body_bytes_sent = self.body_bytes_sent.saturating_add(slice.len());
        write_logical(client, slice, chunked).await
    }

    async fn forward_delayed(
        &mut self,
        client: &mut TcpStream,
        slice: &[u8],
        spec: DelaySpec,
        delay_gate: &DelayGate,
    ) -> StreamDecision {
        if self.delay_triggered {
            self.body_bytes_sent = self.body_bytes_sent.saturating_add(slice.len());
            return write_logical(client, slice, spec.chunked).await;
        }
        let remaining = spec.after_bytes.saturating_sub(self.body_bytes_sent);
        if slice.len() < remaining {
            self.body_bytes_sent = self.body_bytes_sent.saturating_add(slice.len());
            return write_logical(client, slice, spec.chunked).await;
        }
        let (head, tail) = slice.split_at(remaining);
        if !head.is_empty()
            && !matches!(
                write_logical(client, head, spec.chunked).await,
                StreamDecision::Continue
            )
        {
            return StreamDecision::CloseEof;
        }
        self.body_bytes_sent = self.body_bytes_sent.saturating_add(head.len());
        self.delay_triggered = true;
        for _ in 0..spec.yield_steps {
            tokio::task::yield_now().await;
        }
        if spec.wait_for_gate {
            delay_gate.enter_and_wait().await;
        }
        if tail.is_empty() {
            return StreamDecision::Continue;
        }
        self.body_bytes_sent = self.body_bytes_sent.saturating_add(tail.len());
        write_logical(client, tail, spec.chunked).await
    }
}

#[derive(Debug, Clone, Copy)]
struct DelaySpec {
    after_bytes: usize,
    yield_steps: u32,
    wait_for_gate: bool,
    chunked: bool,
}

async fn write_logical(client: &mut TcpStream, data: &[u8], chunked: bool) -> StreamDecision {
    if data.is_empty() {
        return StreamDecision::Continue;
    }
    if chunked {
        let encoded = encode_http_chunk(data);
        write_and_flush(client, &encoded).await
    } else {
        write_and_flush(client, data).await
    }
}

async fn write_and_flush(client: &mut TcpStream, data: &[u8]) -> StreamDecision {
    if data.is_empty() {
        return StreamDecision::Continue;
    }
    if client.write_all(data).await.is_err() || client.flush().await.is_err() {
        StreamDecision::CloseEof
    } else {
        StreamDecision::Continue
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
