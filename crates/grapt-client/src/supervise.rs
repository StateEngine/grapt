//! One supervised connection: a single explicit state for an attachment to the
//! service, owned by one writer, so input, reads and the view each ask one
//! question instead of combining flags written from different threads.
//!
//! An attachment is the verified RPC peer plus its live event stream. Losing
//! the stream (a gap) keeps the attachment: commands still work while the view
//! resynchronises. A closed attachment refuses commands until a verified
//! replacement is installed. A service that answers as a different product is
//! refused, never trusted. The state is transport-neutral so other supervised
//! sources (mail, webhook or MQTT listeners) can reuse it.
use std::time::Duration;
use tokio::time::Instant;

/// Where one attachment stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    /// Attached and streaming.
    Live,
    /// The stream was lost; the attachment still serves commands while a
    /// replacement stream is established.
    Resyncing { since: Instant },
    /// The attachment closed; a replacement is being verified.
    Replacing { attempt: u32 },
    /// No replacement could be attached; recovery keeps retrying.
    Disconnected { attempt: u32, reason: String },
    /// The service answered but could not be verified as this product.
    Refused { reason: String },
}

/// What happened to the attachment, in the order its owner observed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The live stream lost events; the attachment itself is intact.
    Lost,
    /// The attachment's stream ended or its peer went away.
    Closed,
    /// A replacement attempt started.
    Replacing,
    /// A verified replacement was installed.
    Attached,
    /// A replacement attempt failed to connect or subscribe.
    Failed { reason: String },
    /// A replacement answered as a different or unverifiable product.
    Unverified { reason: String },
}

/// The small, typed view a renderer needs; nothing else leaks out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionView {
    pub state: Connection,
    /// Time since the last live event arrived, when one has.
    pub last_event_age: Option<Duration>,
}

impl Connection {
    /// The state after `event`, observed at `now`.
    #[must_use]
    pub fn next(&self, event: Event, now: Instant) -> Self {
        match (self, event) {
            // A refused service stays refused until a replacement verifies;
            // stream noise from the old attachment cannot revive it.
            (Self::Refused { reason }, Event::Lost | Event::Closed) => Self::Refused {
                reason: reason.clone(),
            },
            (_, Event::Attached) => Self::Live,
            (_, Event::Unverified { reason }) => Self::Refused { reason },
            // A gap only matters while the attachment is live; one already
            // being replaced keeps its stricter state.
            (Self::Live, Event::Lost) => Self::Resyncing { since: now },
            (state, Event::Lost) => state.clone(),
            (Self::Live | Self::Resyncing { .. }, Event::Closed) => Self::Replacing { attempt: 1 },
            (state, Event::Closed) => state.clone(),
            (Self::Disconnected { attempt, .. }, Event::Replacing) => Self::Replacing {
                attempt: attempt.saturating_add(1),
            },
            // Recovery keeps probing a refused service, since the right one
            // may come back on the same profile, but the refusal and its
            // reason stand until a replacement verifies.
            (Self::Refused { reason }, Event::Replacing) => Self::Refused {
                reason: reason.clone(),
            },
            (state, Event::Replacing) => state.clone(),
            (Self::Replacing { attempt }, Event::Failed { reason }) => Self::Disconnected {
                attempt: *attempt,
                reason,
            },
            (Self::Disconnected { attempt, .. }, Event::Failed { reason }) => Self::Disconnected {
                attempt: *attempt,
                reason,
            },
            // A failed resync loses the attachment too: the service may have
            // restarted as something else, so commands must stop.
            (Self::Live | Self::Resyncing { .. }, Event::Failed { reason }) => {
                Self::Disconnected { attempt: 1, reason }
            }
            (Self::Refused { reason }, Event::Failed { .. }) => Self::Refused {
                reason: reason.clone(),
            },
        }
    }

    /// One-shot commands (submit, stop) need only a valid attachment; a gap
    /// in the live view never blocks input.
    #[must_use]
    pub fn accepts_commands(&self) -> bool {
        matches!(self, Self::Live | Self::Resyncing { .. })
    }

    /// Multi-step reads and operator work wait while any replacement is
    /// pending: one installed mid-operation would orphan its reply.
    #[must_use]
    pub fn accepts_incremental(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// Whether live events are flowing from the current attachment.
    #[must_use]
    pub fn streaming(&self) -> bool {
        matches!(self, Self::Live)
    }

    /// Whether recovery has work to do.
    #[must_use]
    pub fn recovering(&self) -> bool {
        !matches!(self, Self::Live)
    }
}

#[cfg(test)]
mod tests;
