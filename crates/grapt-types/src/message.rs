//! Message identity. Law: `PRESERVE` — retry identity is a MessageId, not text
//! (oracle evidence `SessionMessage.fromEvent`).

crate::id::prefixed_id! {
    /// Validated `msg_*` message identity.
    MessageId, "msg"
}
