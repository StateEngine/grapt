//! Streaming and independent incremental request lanes for the real transport.
use super::*;
use crate::contract::Operation;

impl Connection for HttpConnection {
    fn subscribed(&self) -> bool {
        HttpConnection::subscribed(self)
    }
    /// Name the watched Session on the trait path as well as the inherent one.
    ///
    /// Without this override a `Client<HttpConnection>` reached the trait's
    /// default, which does nothing, so the Session the caller selected was
    /// never named on `/api/event` and the window held no lease at all.
    fn watch_session(&mut self, session: Option<grapt_types::SessionId>) {
        HttpConnection::watch_session(self, session);
    }
    fn stream_binding(&self) -> Option<crate::connection::StreamBinding> {
        self.open_binding()
    }
    fn resume_from(&mut self, token: Option<crate::connection::ResumeToken>) {
        self.offer_resume(token);
    }
    fn begin_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        self.start_reconnect()
    }
    fn poll_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        self.advance_reconnect()
    }
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.pending_read = Some(self.start_pending(request, false)?);
        Ok(None)
    }
    fn poll_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        let result = self
            .pending_read
            .as_mut()
            .ok_or(TransportError::NotConnected)?
            .poll();
        if !matches!(result, Ok(None)) {
            self.pending_read = None;
        }
        result
    }
    fn cancel_read(&mut self) {
        self.pending_read = None;
    }
    fn begin_catalog(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        let pending = self.start_pending(request, false)?;
        self.pending_catalog = Some(if request.operation() == Operation::CommandInspect {
            pending.with_mcp_startup(self.startup_active_allowance)
        } else {
            pending
        });
        Ok(None)
    }
    fn poll_catalog(&mut self) -> Result<Option<WireResponse>, TransportError> {
        let result = self
            .pending_catalog
            .as_mut()
            .ok_or(TransportError::NotConnected)?
            .poll();
        if !matches!(result, Ok(None)) {
            self.pending_catalog = None;
        }
        result
    }
    fn catalog_human_wait(&mut self, waiting: bool) {
        if let Some(pending) = &mut self.pending_catalog {
            pending.human_wait(waiting);
        }
    }
    fn cancel_catalog(&mut self) {
        self.pending_catalog = None;
    }
    fn begin_operator(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        let pending = self.start_pending(request, false)?;
        self.pending_operator = Some(
            if matches!(
                request.operation(),
                Operation::McpList | Operation::McpInspect
            ) {
                // Only observed startup forms spend the shared human allowance;
                // ordinary acquisition still has thirty seconds of active time.
                pending.with_mcp_startup(self.startup_active_allowance)
            } else if request.operation() == Operation::ClaudePlugin {
                pending.with_timeout(grapt_api::u3a::CLAUDE_PLUGIN_ALLOWANCE)
            } else {
                pending.with_timeout(Duration::from_secs(80))
            },
        );
        Ok(None)
    }
    fn operator_human_wait(&mut self, waiting: bool) {
        if let Some(pending) = &mut self.pending_operator {
            pending.human_wait(waiting);
        }
    }
    fn poll_operator(&mut self) -> Result<Option<WireResponse>, TransportError> {
        let result = self
            .pending_operator
            .as_mut()
            .ok_or(TransportError::NotConnected)?
            .poll();
        if !matches!(result, Ok(None)) {
            self.pending_operator = None;
        }
        result
    }
    fn cancel_operator(&mut self) {
        self.pending_operator = None;
    }

    fn begin_tool_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.pending_tools = Some(self.start_pending(request, false)?);
        Ok(None)
    }
    fn poll_tool_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        let result = self
            .pending_tools
            .as_mut()
            .ok_or(TransportError::NotConnected)?
            .poll();
        if !matches!(result, Ok(None)) {
            self.pending_tools = None;
        }
        result
    }
    fn cancel_tool_read(&mut self) {
        self.pending_tools = None;
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        if let Some(refusal) = self.refusal.take() {
            return Err(refusal);
        }
        // A ready frame first. Draining what the reader already parsed before
        // touching the socket keeps one slow read from stalling frames that
        // have already arrived.
        if let Some(frame) = self.buffered_frame()? {
            return Ok(frame);
        }
        let Some(stream) = self.stream.as_mut() else {
            return Ok(FramePoll::Closed);
        };
        let mut chunk = [0u8; 8_192];
        match stream.read(&mut chunk) {
            Ok(0) => {
                // The subscription ended. ORDINARY, per the contract: live
                // delivery is advisory, and the client reconciles.
                self.closed()
            }
            Ok(count) => {
                self.push_stream_bytes(&chunk[..count])?;
                Ok(self.buffered_frame()?.unwrap_or(FramePoll::Idle))
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                // No frame is ready. Keep the subscription open and let the
                // terminal own the idle wait, so queued input is not delayed.
                Ok(FramePoll::Idle)
            }
            Err(_) => {
                // An ABRUPT end is still an end, and this client's recovery for
                // it is identical to a graceful one: drop the subscription and
                // reconcile from durable truth.
                //
                // This used to return an error, which `pump` propagated with
                // `?` and the app treated as fatal — so a connection reset KILLED
                // THE CLIENT. Measured: with a relay between client and product,
                // the native client exited to its banner mid-journey, while the
                // retained client carried on. The contract makes this ordinary
                // rather than exceptional: `/api/event` is volatile, a slow
                // consumer overflows and fails the stream, and events during a
                // disconnection are missed. A client that dies when the thing
                // it was promised may happen, happens, is the defect.
                //
                // A read error that is NOT the subscription — a request socket —
                // still surfaces as an error, because those are call failures
                // the caller must see.
                self.closed()
            }
        }
    }

    fn resubscribe(&mut self) {
        // Best-effort by design: `resubscribe` returns nothing because the
        // client's recovery is identical whether or not it succeeds. A failed
        // resubscribe leaves the client reconciling from durable truth, which
        // is exactly where a client with no live stream belongs.
        self.stream = None;
        self.reader = SseReader::new(self.bounds);
        if self.identity_pending {
            if let Some(expected) = &self.expected_product {
                if self.verify_product(expected).is_err() {
                    return;
                }
            }
            self.identity_pending = false;
        }
        // A refusing status (an older service's 400 for positions among them)
        // is reported by the next poll, never mistaken for "not subscribed";
        // a transport failure still reads as a closed stream, as before.
        match self.open_stream() {
            Ok(()) => {}
            Err(
                refusal @ (TransportError::Status { .. } | TransportError::ServerTooOld { .. }),
            ) => self.refusal = Some(refusal),
            Err(_) => {}
        }
    }

    fn retire_stream(&mut self) {
        self.stream = None;
        self.reader = SseReader::new(self.bounds);
    }
}

impl HttpConnection {
    /// One subscribe attempt: a stream, or why there is none.
    fn open_stream(&mut self) -> Result<(), TransportError> {
        let deadline = tokio::time::Instant::now() + READ_TIMEOUT;
        let request = self.subscribe_request()?;
        let socket = self.connect(STREAM_CONNECT_TIMEOUT)?;
        let mut stream = window::SocketWindow {
            stream: socket,
            deadline,
        };
        self.write_request(
            &mut stream,
            Method::Get,
            request.path(),
            request.query(),
            None,
        )?;
        // The head is read with the full timeout, because a subscription that
        // has not yet answered is not idle — it is still being established.
        let head = Self::read_head(&mut stream)?;
        if !(200..300).contains(&head.status) {
            return Err(super::recovery::subscribe_refusal(head.status));
        }
        self.reader = self.stream_reader();
        self.stream_binding = Some(self.binding_for());
        // Whatever arrived with the head is already stream content.
        self.push_stream_bytes(&head.body_prefix)?;
        let stream = stream.stream;
        // Request/header acquisition above keeps its bounded blocking window.
        // Established advisory streams only drain available data, like the
        // existing incremental reconnect path; input owns the idle wait.
        stream
            .set_nonblocking(true)
            .map_err(|error| TransportError::Io {
                reason: error.to_string(),
            })?;
        self.stream = Some(stream);
        Ok(())
    }
}
