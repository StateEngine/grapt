//! Bidirectional cursor metadata from the bounded message-page DTO.
use super::{DurableMessage, MessageWireId};

/// What a decoded re-list page yielded.
#[derive(Debug, Clone, PartialEq)]
pub struct MessagePage {
    pub previous: Option<String>,
    pub messages: Vec<DurableMessage>,
    /// The cursor to ask for the next page with, if the server offered one.
    ///
    /// Presence is an OFFER to page, not a claim that there is more. What
    /// earns liveness is following it to exhaustion.
    pub next: Option<String>,
    /// Rows or content items this client cannot PRESENT, though they decoded.
    ///
    /// Never silently zero: the transcript says what it is not showing. It does
    /// NOT mean the history is incomplete, and it must never be used to claim
    /// that — the rows were read, they are simply not drawn.
    pub unsupported: usize,
    /// How many rows the wire page actually carried, before this client
    /// dropped any it cannot present.
    ///
    /// The EXHAUSTION SIGNAL, and it has to come from the raw page: a page
    /// carrying only unsupported rows decodes to zero messages, and a walk that
    /// read "no messages" as "no more history" stopped early while the server
    /// was still offering a cursor — silently truncating the transcript and
    /// then blaming the rows it had discarded.
    pub rows: usize,
    /// Durable row identities aligned with messages, used by the tail owner.
    pub ids: Vec<MessageWireId>,
}
