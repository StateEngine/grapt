//! A service too old for this client is refused once, and from then on it is
//! asked nothing: every request this client would start is refused, and the
//! work already waiting on the service settles with the same refusal.
use super::*;

impl<C: Connection> Client<C> {
    /// The refusal, once this service has been refused as too old.
    pub(super) fn refused(&self) -> Result<(), ClientError> {
        self.recovery
            .too_old
            .map_or(Ok(()), |missing| Err(too_old(missing)))
    }

    /// Refuse this service terminally as too old for this client. Its stream
    /// is retired before the refusal, so nothing it sends later can count.
    pub(super) fn refuse_too_old(&mut self, missing: &'static str) -> ClientError {
        self.connection.cancel_read();
        self.connection.refuse_stream(missing);
        self.positions.reset();
        self.cursor.stream_closed();
        self.settle_refused(missing)
    }

    /// Latch the refusal and settle the reads only the pump advances, which
    /// it no longer does: queued history, a preset read, the catalog lane's
    /// agent, model, repository and command reads, an MCP catalog read and a
    /// tool refresh. A request its host polls settles at its next poll.
    pub(super) fn settle_refused(&mut self, missing: &'static str) -> ClientError {
        self.recovery.too_old = Some(missing);
        self.recovery.history = None;
        let refusal = too_old(missing).to_string();
        self.refuse_preset(&refusal);
        self.refuse_catalog(&refusal);
        self.refuse_commands(&refusal);
        self.refuse_mcp_catalog(refusal);
        self.cancel_tool_refresh();
        too_old(missing)
    }

    /// A too-old refusal from any read becomes terminal; others pass through.
    pub(super) fn latch_too_old(&mut self, error: ClientError) -> ClientError {
        match error {
            ClientError::Transport(TransportError::ServerTooOld { missing }) => {
                self.refuse_too_old(missing)
            }
            error => error,
        }
    }

    /// Start an operator request, unless the service was refused.
    pub(super) fn begin_operator(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, ClientError> {
        self.refused()?;
        self.connection
            .begin_operator(request)
            .map_err(ClientError::from)
    }

    /// Poll the operator request; one the service was refused under ends
    /// with the refusal.
    pub(super) fn poll_operator(&mut self) -> Result<Option<WireResponse>, ClientError> {
        if let Err(refusal) = self.refused() {
            self.connection.cancel_operator();
            return Err(refusal);
        }
        self.connection.poll_operator().map_err(ClientError::from)
    }
}

/// The refusal for a service that predates what this client needs.
pub(super) fn too_old(missing: &'static str) -> ClientError {
    ClientError::Transport(TransportError::ServerTooOld { missing })
}
