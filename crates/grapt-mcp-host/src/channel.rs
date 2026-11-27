// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: serial channel deadline accounting, bounded nested
// requests and existing form cancellation; transport shares the scoped pause.
// (active_time_timeout), commit 092d3acd6bec3e3a14bdc7e7a2810ab628ab759d.
// Modified further: startup-only human lifetime/request cap, cumulative phase
// pause accounting and connection-revision propagation. Timeout/cancellation
// adaptation also inspected at bb64b894fd67158be0f874872e6d7bfe717e7368.

//! The bounded newline-delimited JSON-RPC channel, and what can go wrong on it.
//!
//! Owns framing and deadlines while the application supplies workspace roots,
//! notification delivery and nested server-request handling.

use std::time::Duration;
#[path = "channel_idle.rs"]
mod idle;

use grapt_mcp::session::Unusable;
use grapt_mcp::{
    decode_reply, McpBounds, McpProtocolError, OutboundNotification, OutboundRequest,
    RequestSequence,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[cfg(test)]
#[path = "channel_requests_tests.rs"]
mod request_tests;

/// A display-only observer scoped to one active tool exchange.
pub trait ProgressObserver: Send + Sync {
    fn token(&self) -> &str;
    fn notify(&self, params: &Value);
}

pub trait ServerRequestHandler: Send {
    /// The connection's revision, or the client's offer before initialize replies.
    fn protocol(&mut self, _revision: &str) {}
    /// `dispatch` is the admitted tool call this nested request arrived under.
    ///
    /// PASSED EXPLICITLY RATHER THAN STASHED ON THE HANDLER. The identity was
    /// always alive on this stack — `call_with` receives it and hands it to
    /// `run_call_with`, which holds it across `exchange_with` — and was simply
    /// dropped before reaching the handler. Threading it as an argument keeps
    /// it scoped to the one call it describes; a field set before dispatch and
    /// cleared afterwards would be the same value with a window in which it
    /// could be read against the wrong call.
    ///
    /// `None` where no call frames the request (a bare exchange, or a test that
    /// drives the handler directly), never a placeholder identity.
    fn answer<'a>(
        &'a mut self,
        request: &'a grapt_mcp::incoming::ServerRequest,
        dispatch: Option<&'a grapt_form::global::CallDispatch>,
    ) -> crate::RequestFuture<'a>;
    fn progress(&self) -> Option<std::sync::Arc<dyn ProgressObserver>> {
        None
    }
    fn cancelled(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// What went wrong on the wire. Each variant maps to exactly one
/// [`Unusable`] state, so the transport cannot report a timeout as a failure
/// or a hangup as a timeout.
#[derive(Debug)]
pub enum ChannelError {
    /// The deadline passed. Nothing is known about whether the server acted.
    TimedOut,
    /// The stream ended or errored. The channel is provably gone.
    Disconnected(String),
    /// Bytes arrived and could not be used.
    Protocol(McpProtocolError),
}

impl ChannelError {
    pub fn into_unusable(self) -> Unusable {
        match self {
            Self::TimedOut => Unusable::TimedOut,
            Self::Disconnected(detail) => {
                // Kept distinct from a timeout: a closed channel is a fact,
                // while a deadline is an absence of one.
                let _ = &detail;
                Unusable::Disconnected
            }
            Self::Protocol(error) => Unusable::Failed(error.to_string()),
        }
    }
}

/// One newline-delimited JSON-RPC channel.
///
/// MCP's stdio transport frames one JSON object per line. Reading a line is
/// the obvious operation and the dangerous one: a server that never emits a
/// newline would otherwise buy unbounded memory from us, so the read is capped
/// at the declared frame bound and a longer line is a protocol refusal rather
/// than a larger allocation.
#[derive(Debug)]
pub struct McpChannel<R, W> {
    reader: BufReader<R>,
    writer: W,
    bounds: McpBounds,
    sequence: RequestSequence,
    partial_frame: Vec<u8>,
    /// Stays set if a write is dropped, times out or errors. Appending another
    /// frame could corrupt framing or repeat an effect whose outcome is unknown.
    write_in_progress: bool,
    human_wait: Option<grapt_mcp_client::HumanWait>,
    /// The request whose reply this channel has not yet consumed.
    outstanding: Option<u64>,
    /// Requests abandoned before their reply (an interrupt dropped the
    /// exchange, or its deadline passed). A late reply to one of these is
    /// discarded; any other uncorrelated reply still refuses the channel.
    abandoned: std::collections::VecDeque<u64>,
    startup_trace: Option<crate::startup_trace::StartupTrace>,
    catalog_signal: Option<std::sync::Arc<dyn crate::CatalogNotifications>>,
    roots: Option<std::sync::Arc<dyn crate::WorkspaceRoots>>,
    idle_reply: Option<(Vec<u8>, usize)>,
    negotiated_protocol: Option<&'static str>,
    elicitation_enabled: bool,
    human_paused: Duration,
    startup_human_remaining: Option<Duration>,
    startup_requests_remaining: Option<usize>,
    startup_lease: Option<std::sync::Arc<crate::startup_lease::StartupLease>>,
}

/// The oldest abandoned id is forgotten past this bound, so a reply that late
/// fails closed as uncorrelated rather than growing the set.
const MAX_ABANDONED: usize = 32;

impl<R, W> McpChannel<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W, bounds: McpBounds) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            bounds,
            sequence: RequestSequence::new(),
            partial_frame: Vec::new(),
            write_in_progress: false,
            human_wait: None,
            outstanding: None,
            abandoned: std::collections::VecDeque::new(),
            startup_trace: None,
            catalog_signal: None,
            roots: None,
            idle_reply: None,
            negotiated_protocol: None,
            elicitation_enabled: false,
            human_paused: Duration::ZERO,
            startup_human_remaining: None,
            startup_requests_remaining: None,
            startup_lease: None,
        }
    }

    pub fn unsubscribe_request(&mut self, uri: &str) -> Result<OutboundRequest, McpProtocolError> {
        grapt_mcp::content::subscription_request(&mut self.sequence, self.bounds, uri, false)
    }

    pub fn with_startup_trace(mut self, trace: crate::startup_trace::StartupTrace) -> Self {
        self.startup_trace = Some(trace);
        self
    }

    pub fn startup_stage(&self, stage: &'static str) {
        if let Some(trace) = &self.startup_trace {
            trace.mark(stage);
        }
    }

    pub fn finish_startup(&mut self) {
        self.startup_lease = None;
        self.startup_human_remaining = None;
        self.startup_requests_remaining = None;
        if let Some(trace) = self.startup_trace.take() {
            trace.mark("catalog");
        }
    }

    pub fn with_startup_lease(
        mut self,
        startup: std::sync::Arc<crate::startup_lease::StartupLease>,
    ) -> Self {
        self.startup_lease = Some(startup);
        self
    }

    fn check_startup_live(&self) -> Result<(), ChannelError> {
        if self
            .startup_lease
            .as_ref()
            .is_some_and(|lease| lease.is_retired())
        {
            return Err(ChannelError::Disconnected("MCP startup was retired".into()));
        }
        Ok(())
    }

    pub fn begin_startup(&mut self, elicitation_enabled: bool) {
        self.negotiated_protocol = None;
        self.elicitation_enabled = elicitation_enabled;
        self.startup_human_remaining = Some(Duration::from_secs(10 * 60));
        self.startup_requests_remaining = Some(32);
    }

    #[cfg(test)]
    fn with_elicitation_for_test(mut self) -> Self {
        // Direct channel fixtures omit initialize; real startup records the
        // capability in both its initialize frame and this channel's state.
        self.elicitation_enabled = true;
        self
    }

    pub fn set_protocol(&mut self, revision: &'static str) {
        self.negotiated_protocol = Some(revision);
    }

    pub fn human_paused(&self) -> Duration {
        self.human_paused
    }

    pub fn active_elapsed(
        &self,
        started: tokio::time::Instant,
        paused_before: Duration,
    ) -> Duration {
        started
            .elapsed()
            .saturating_sub(self.human_paused.saturating_sub(paused_before))
    }

    fn admit_startup_request(&mut self) -> Result<(), ChannelError> {
        if let Some(remaining) = &mut self.startup_requests_remaining {
            *remaining = remaining.checked_sub(1).ok_or_else(|| {
                ChannelError::Protocol(McpProtocolError::Malformed(
                    "startup server request bound exceeded".into(),
                ))
            })?;
        }
        Ok(())
    }

    pub fn with_human_wait(mut self, human_wait: grapt_mcp_client::HumanWait) -> Self {
        self.human_wait = Some(human_wait);
        self
    }

    pub fn sequence(&mut self) -> &mut RequestSequence {
        &mut self.sequence
    }

    /// Attach the application's coalesced catalog and resource notifications.
    pub fn set_catalog_signal(&mut self, signal: std::sync::Arc<dyn crate::CatalogNotifications>) {
        self.catalog_signal = Some(signal);
    }

    pub fn catalog_signal(&self) -> Option<&std::sync::Arc<dyn crate::CatalogNotifications>> {
        self.catalog_signal.as_ref()
    }

    pub fn with_roots(mut self, roots: std::sync::Arc<dyn crate::WorkspaceRoots>) -> Self {
        self.roots = Some(roots);
        self
    }

    fn roots_response(
        &self,
        request: &grapt_mcp::incoming::ServerRequest,
    ) -> Result<Vec<u8>, McpProtocolError> {
        match self.roots.as_ref().map(|roots| roots.response()) {
            Some(Ok(result)) => request.result(self.bounds, result).or_else(|_| {
                request.error(
                    self.bounds,
                    -32603,
                    "Workspace roots exceed the response bound",
                )
            }),
            None => request.result(self.bounds, serde_json::json!({"roots":[]})),
            Some(Err(_)) => request.error(self.bounds, -32603, "Workspace roots are unavailable"),
        }
    }

    /// Send one request and read the reply that answers it, under one deadline.
    ///
    /// The deadline covers the whole exchange rather than each syscall: a
    /// server that dribbles one byte per interval would otherwise satisfy a
    /// per-read deadline forever. Only the admitted human elicitation wait is
    /// excluded; reads, writes and nested protocol work share one allowance.
    pub async fn exchange(
        &mut self,
        request: OutboundRequest,
        deadline: Duration,
    ) -> Result<Value, ChannelError> {
        self.exchange_with(request, deadline, None, None).await
    }

    pub async fn exchange_with(
        &mut self,
        request: OutboundRequest,
        deadline: Duration,
        handler: Option<&mut dyn ServerRequestHandler>,
        dispatch: Option<&grapt_form::global::CallDispatch>,
    ) -> Result<Value, ChannelError> {
        let deadline = tokio::time::Instant::now()
            .checked_add(deadline)
            .ok_or(ChannelError::TimedOut)?;
        self.exchange_inner(&request, deadline, handler, dispatch)
            .await
    }

    /// Send one notification and read nothing.
    ///
    /// A notification has no id, so there is no reply to wait for and nothing
    /// to correlate. Waiting anyway would consume the next frame the server
    /// sends and hand it to whichever request asked next, so the absence of a
    /// read here is the point rather than an omission. The deadline still
    /// applies, because a server that never drains its input can block the
    /// write itself.
    pub async fn notify(
        &mut self,
        notification: OutboundNotification,
        deadline: Duration,
    ) -> Result<(), ChannelError> {
        match tokio::time::timeout(deadline, self.write_frame(notification.frame())).await {
            Err(_) => Err(ChannelError::TimedOut),
            Ok(result) => result,
        }
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn exchange_inner(
        &mut self,
        request: &OutboundRequest,
        mut deadline: tokio::time::Instant,
        mut handler: Option<&mut dyn ServerRequestHandler>,
        dispatch: Option<&grapt_form::global::CallDispatch>,
    ) -> Result<Value, ChannelError> {
        self.check_startup_live()?;
        if let Some(handler) = handler.as_deref_mut() {
            handler.protocol(
                self.negotiated_protocol
                    .unwrap_or(grapt_mcp::PROTOCOL_VERSION),
            );
        }
        if let Some(abandoned) = self.outstanding.take() {
            if self.abandoned.len() == MAX_ABANDONED {
                self.abandoned.pop_front();
            }
            self.abandoned.push_back(abandoned);
        }
        self.outstanding = Some(request.id().value());
        tokio::time::timeout_at(deadline, self.write_frame(request.frame()))
            .await
            .map_err(|_| ChannelError::TimedOut)??;
        let progress = handler.as_ref().and_then(|handler| handler.progress());
        let mut seen = Vec::new();
        let mut notifications = 0usize;
        loop {
            let frame = tokio::time::timeout_at(deadline, self.read_frame())
                .await
                .map_err(|_| ChannelError::TimedOut)??;
            if let Some(notification) = grapt_mcp::incoming::notification(self.bounds, &frame)
                .map_err(ChannelError::Protocol)?
            {
                self.admit_startup_request()?;
                if notification["method"] == "notifications/progress" {
                    if let Some(progress) = &progress {
                        progress.notify(&notification["params"]);
                    }
                    // Constant storage and the original exchange deadline bound
                    // advisory updates; yielding keeps a flooding peer cooperative.
                    tokio::task::yield_now().await;
                    continue;
                }
                if let Some(signal) = &self.catalog_signal {
                    signal.accept(&notification);
                }
                notifications += 1;
                if notifications > 32 {
                    return Err(ChannelError::Protocol(McpProtocolError::Malformed(
                        "server notification bound".into(),
                    )));
                }
                continue;
            }
            let Some(incoming) = grapt_mcp::incoming::server_request(self.bounds, &frame)
                .map_err(ChannelError::Protocol)?
            else {
                match decode_reply(self.bounds, request.id(), &frame) {
                    Err(McpProtocolError::CorrelationMismatch { replied, .. })
                        if self.forget_abandoned(replied) =>
                    {
                        continue;
                    }
                    reply => {
                        self.outstanding = None;
                        return reply.map_err(ChannelError::Protocol);
                    }
                }
            };
            self.admit_startup_request()?;
            if seen.len() >= 32 || seen.contains(incoming.id()) {
                return Err(ChannelError::Protocol(McpProtocolError::Malformed(
                    "repeated or excessive nested server request".into(),
                )));
            }
            seen.push(incoming.id().clone());
            let response = if incoming.method() == "ping" {
                incoming.result(self.bounds, serde_json::json!({}))
            } else if incoming.method() == "roots/list" {
                self.roots_response(&incoming)
            } else if incoming.method() == "elicitation/create" {
                self.check_startup_live()?;
                if !self.elicitation_enabled {
                    incoming.error(self.bounds, -32602, "Elicitation was not advertised")
                } else if let Some(handler) = handler.as_deref_mut() {
                    // Human response time is not server execution time. Keep
                    // the original remaining budget after this one wait; a
                    // second form never renews the server's active allowance.
                    let transport_pause = self
                        .human_wait
                        .as_ref()
                        .map(|wait| wait.enter())
                        .transpose()
                        .map_err(|error| ChannelError::Disconnected(error.to_string()))?;
                    // The same scoped pause reaches recovery-round and queued
                    // admission timers, not only this channel and remote I/O.
                    let startup_wait = self
                        .startup_lease
                        .as_ref()
                        .map(|lease| lease.human_wait().clone());
                    let startup_pause = startup_wait
                        .as_ref()
                        .map(|wait| wait.enter())
                        .transpose()
                        .map_err(|error| ChannelError::Disconnected(error.to_string()))?;
                    let server_pause = self
                        .startup_lease
                        .as_ref()
                        .map(|lease| lease.server_human_wait().enter())
                        .transpose()
                        .map_err(|error| ChannelError::Disconnected(error.to_string()))?;
                    let paused = tokio::time::Instant::now();
                    let remaining = self.startup_human_remaining.map(|remaining| {
                        startup_wait.as_ref().map_or(remaining, |wait| {
                            remaining.min(
                                Duration::from_secs(10 * 60).saturating_sub(wait.paused_elapsed()),
                            )
                        })
                    });
                    let answer = match remaining {
                        Some(remaining) => tokio::time::timeout(
                            remaining,
                            self.await_answer(handler, &incoming, dispatch, progress.as_deref()),
                        )
                        .await
                        .map_err(|_| {
                            ChannelError::Protocol(McpProtocolError::Malformed(
                                "startup elicitation human wait expired".into(),
                            ))
                        })?,
                        None => {
                            self.await_answer(handler, &incoming, dispatch, progress.as_deref())
                                .await
                        }
                    };
                    let elapsed = paused.elapsed();
                    self.human_paused = self.human_paused.saturating_add(elapsed);
                    if let Some(remaining) = &mut self.startup_human_remaining {
                        *remaining = remaining.saturating_sub(elapsed);
                    }
                    let answer = answer?;
                    self.check_startup_live()?;
                    drop(startup_pause);
                    drop(server_pause);
                    drop(transport_pause);
                    deadline = deadline
                        .checked_add(elapsed)
                        .ok_or(ChannelError::TimedOut)?;
                    let Some(answer) = answer else {
                        continue;
                    };
                    match answer {
                        Ok(result) => incoming.result(self.bounds, result),
                        Err(_) => incoming.error(
                            self.bounds,
                            -32602,
                            "The elicitation request could not be accepted",
                        ),
                    }
                } else {
                    incoming.error(
                        self.bounds,
                        -32601,
                        "Elicitation is unavailable for this operation",
                    )
                }
            } else {
                incoming.error(self.bounds, -32601, "Unsupported server method")
            }
            .map_err(ChannelError::Protocol)?;
            tokio::time::timeout_at(deadline, self.write_frame(&response))
                .await
                .map_err(|_| ChannelError::TimedOut)??;
        }
    }

    fn forget_abandoned(&mut self, replied: u64) -> bool {
        let position = self.abandoned.iter().position(|id| *id == replied);
        position.map(|index| self.abandoned.remove(index)).is_some()
    }

    async fn await_answer(
        &mut self,
        handler: &mut dyn ServerRequestHandler,
        request: &grapt_mcp::incoming::ServerRequest,
        dispatch: Option<&grapt_form::global::CallDispatch>,
        progress: Option<&dyn ProgressObserver>,
    ) -> Result<Option<Result<Value, String>>, ChannelError> {
        let mut waiting = handler.answer(request, dispatch);
        let mut notifications = 0;
        while notifications < 32 {
            let frame = tokio::select! {
                answer = &mut waiting => return Ok(Some(answer)),
                frame = self.read_frame() => frame?,
            };
            let Some(notification) = grapt_mcp::incoming::notification(self.bounds, &frame)
                .map_err(ChannelError::Protocol)?
            else {
                if let Some(request) = grapt_mcp::incoming::server_request(self.bounds, &frame)
                    .map_err(ChannelError::Protocol)?
                    .filter(|request| request.method() == "roots/list")
                {
                    self.admit_startup_request()?;
                    let response = self
                        .roots_response(&request)
                        .map_err(ChannelError::Protocol)?;
                    tokio::time::timeout(Duration::from_secs(5), self.write_frame(&response))
                        .await
                        .map_err(|_| ChannelError::TimedOut)??;
                    notifications += 1;
                    continue;
                }
                return Err(ChannelError::Protocol(McpProtocolError::Malformed(
                    "server sent a request or reply before settling its elicitation".into(),
                )));
            };
            self.admit_startup_request()?;
            if notification["method"] == "notifications/progress" {
                if let Some(progress) = progress {
                    progress.notify(&notification["params"]);
                }
                tokio::task::yield_now().await;
                continue;
            }
            notifications += 1;
            if let Some(signal) = &self.catalog_signal {
                signal.accept(&notification);
            }
            if notification["method"] == "notifications/cancelled"
                && notification["params"]["requestId"] == *request.id()
            {
                drop(waiting);
                handler
                    .cancelled()
                    .map_err(|error| ChannelError::Protocol(McpProtocolError::Malformed(error)))?;
                return Ok(None);
            }
        }
        Err(ChannelError::Protocol(McpProtocolError::Malformed(
            "server notification bound while awaiting elicitation".into(),
        )))
    }

    async fn write_frame(&mut self, frame: &[u8]) -> Result<(), ChannelError> {
        std::future::poll_fn(|cx| self.poll_idle_reply(cx)).await?;
        if self.write_in_progress {
            return Err(ChannelError::Disconnected(
                "the previous frame write did not finish; reconnect before retrying".into(),
            ));
        }
        self.write_in_progress = true;
        self.writer
            .write_all(frame)
            .await
            .map_err(|error| ChannelError::Disconnected(error.to_string()))?;
        self.writer
            .write_all(b"\n")
            .await
            .map_err(|error| ChannelError::Disconnected(error.to_string()))?;
        self.writer
            .flush()
            .await
            .map_err(|error| ChannelError::Disconnected(error.to_string()))?;
        self.write_in_progress = false;
        if let Some(trace) = &mut self.startup_trace {
            trace.wrote_initialize();
        }
        Ok(())
    }

    /// Read one bounded newline-delimited frame.
    async fn read_frame(&mut self) -> Result<Vec<u8>, ChannelError> {
        let limit = self.bounds.max_frame_bytes();
        loop {
            // Partial bytes belong to the channel, so cancelling a read when
            // the form answer arrives cannot discard half a notification.
            let available = self
                .reader
                .fill_buf()
                .await
                .map_err(|error| ChannelError::Disconnected(error.to_string()))?;
            if available.is_empty() {
                return Err(ChannelError::Disconnected(
                    if self.partial_frame.is_empty() {
                        "the server closed the channel".into()
                    } else {
                        "the server ended the channel mid-frame".into()
                    },
                ));
            }
            if let Some(trace) = &mut self.startup_trace {
                trace.read_stdout();
            }
            let through_newline = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| index + 1)
                .unwrap_or(available.len());
            let take = through_newline.min(
                limit
                    .saturating_add(1)
                    .saturating_sub(self.partial_frame.len()),
            );
            self.partial_frame.reserve_exact(take);
            self.partial_frame.extend_from_slice(&available[..take]);
            self.reader.consume(take);
            if self.partial_frame.last() == Some(&b'\n') {
                self.partial_frame.pop();
                return Ok(std::mem::take(&mut self.partial_frame));
            }
            if self.partial_frame.len() > limit || take == 0 {
                return Err(ChannelError::Protocol(McpProtocolError::FrameTooLarge {
                    limit,
                    offered: self.partial_frame.len(),
                }));
            }
        }
    }
}
