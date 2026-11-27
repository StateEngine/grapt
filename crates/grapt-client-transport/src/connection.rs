//! The connection a client drives: a transport that can also stream.
use crate::transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse};

/// What a stream was opened for: its endpoint, credential, watched Session and
/// position class. A resume may continue a stream only under the same binding.
/// Connections build it; the client only compares.
#[derive(Clone, PartialEq, Eq)]
pub struct StreamBinding {
    endpoint: String,
    /// A fingerprint, never the credential itself.
    credential: u64,
    session: Option<grapt_types::SessionId>,
    positions: bool,
}

impl StreamBinding {
    pub fn new(
        endpoint: impl Into<String>,
        credential: u64,
        session: Option<grapt_types::SessionId>,
        positions: bool,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            credential,
            session,
            positions,
        }
    }
}

impl core::fmt::Debug for StreamBinding {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("StreamBinding")
            .field("endpoint", &self.endpoint)
            .field("session", &self.session)
            .field("positions", &self.positions)
            .finish_non_exhaustive()
    }
}

/// A client's offer to resume: the id of the last frame it holds, and the
/// binding of the stream that frame came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeToken {
    pub id: String,
    pub binding: StreamBinding,
}

/// A transport that can also stream. One object owns both because the client
/// needs to issue a command and read the stream in the same breath.
pub trait Connection: Transport {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError>;
    /// Re-open the subscription after it closed.
    fn resubscribe(&mut self);
    fn subscribed(&self) -> bool;
    /// End the current stream now, because it broke its own contract (a
    /// frame of the other position class). The next reconnect opens a fresh
    /// subscription. Every shipped connection implements it; the default
    /// serves test peers that own no stream.
    fn retire_stream(&mut self) {}
    /// The service has no snapshot (404): subscribe without positions from
    /// now until the legacy stream this installs is lost, retiring the
    /// current stream first. The HTTP connection and the bridge latch it;
    /// the default serves connections that never ask for positions.
    fn refuse_positions(&mut self) {
        self.retire_stream();
    }

    /// Name the Session this window is watching, so the service can tell when
    /// its last window is gone. A transport with no live attachment ignores
    /// this, and a later `resubscribe` re-sends the current name.
    fn watch_session(&mut self, _session: Option<grapt_types::SessionId>) {}

    /// The open stream's binding, when one is open and this connection can
    /// resume one. The default serves connections that never resume.
    fn stream_binding(&self) -> Option<StreamBinding> {
        None
    }

    /// Offer, or withdraw, a resume for the next subscribe. A connection sends
    /// `Last-Event-ID` only when the subscribe it opens has the token's binding.
    fn resume_from(&mut self, _token: Option<ResumeToken>) {}

    /// How many frames past the offered id the client already holds, told
    /// beside each offer, so a connection that raises its own per-frame
    /// refreshes can leave that replayed prefix alone. The default ignores it.
    fn resume_suffix(&mut self, _frames: usize) {}

    /// The first frame after an offer was answered: `true` when it certified
    /// the resume, `false` when the client refused it or abandoned the offer.
    /// A connection holding refreshes back for that answer releases them here
    /// exactly once. The default holds nothing.
    fn resume_answered(&mut self, _certified: bool) {}

    /// Recovery I/O may complete immediately (embedded) or on a later poll
    /// (socket). Neither form bypasses the encoded DTO response boundary.
    fn begin_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        self.resubscribe();
        Ok(Some(self.subscribed()))
    }
    fn poll_reconnect(&mut self) -> Result<Option<bool>, TransportError> {
        Ok(Some(self.subscribed()))
    }
    fn begin_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.call(request).map(Some)
    }
    fn poll_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        Err(TransportError::NotConnected)
    }
    fn cancel_read(&mut self) {}

    /// Catalog refresh has its own bounded read, so history and commands can
    /// proceed while a provider catalog is slow.
    fn begin_catalog(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.call(request).map(Some)
    }
    fn poll_catalog(&mut self) -> Result<Option<WireResponse>, TransportError> {
        Err(TransportError::NotConnected)
    }
    fn catalog_human_wait(&mut self, _waiting: bool) {}
    fn cancel_catalog(&mut self) {}

    /// One independent, bounded operator request; event and interrupt work
    /// continues while OAuth discovery or registration waits on a server.
    fn begin_operator(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.call(request).map(Some)
    }
    fn poll_operator(&mut self) -> Result<Option<WireResponse>, TransportError> {
        Err(TransportError::NotConnected)
    }
    fn operator_human_wait(&mut self, _waiting: bool) {}
    fn cancel_operator(&mut self) {}

    /// Tool hints must not block input or occupy the history/catalog lanes.
    fn begin_tool_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        self.call(request).map(Some)
    }
    fn poll_tool_read(&mut self) -> Result<Option<WireResponse>, TransportError> {
        Err(TransportError::NotConnected)
    }
    fn cancel_tool_read(&mut self) {}
}
