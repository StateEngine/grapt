//! A physical operation may fail outside its semantic success/refusal family.
//! Lost/malformed write replies retain debt; they never imply primary absence.
use crate::failure::{
    DeliveryAckFailure, LifecycleOutcome, OpenFailure, SubscriptionClosed, TransportFailure,
};
use crate::identity::RemoteMailboxMessageId;
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteEffect {
    None,
    UnknownRetryExact,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    Absent,
    Incomplete,
    Unreadable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionFailure {
    Unavailable,
    Deadline,
    InvalidEnvelope,
}

/// Closed causes. No foreign JSON, credential or exception string crosses here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationCause {
    Open(OpenFailure),
    Transport(TransportFailure),
    Delivery(DeliveryAckFailure),
    Subscription(SubscriptionClosed),
    Lifecycle(LifecycleOutcome),
    Connection(ConnectionFailure),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialDismiss {
    pub dismissed: Vec<RemoteMailboxMessageId>,
    pub already_dismissed: Vec<RemoteMailboxMessageId>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationFailure {
    pub cause: OperationCause,
    pub effect: WriteEffect,
    pub source_state: Option<SourceState>,
    pub partial_dismiss: Option<PartialDismiss>,
}
impl OperationFailure {
    pub(crate) fn new(cause: OperationCause, effect: WriteEffect) -> Self {
        Self {
            cause,
            effect,
            source_state: None,
            partial_dismiss: None,
        }
    }
}
macro_rules! semantic_failure {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for OperationFailure {
            fn from(value: $ty) -> Self {
                Self::new(OperationCause::$variant(value), WriteEffect::None)
            }
        }
    };
}
semantic_failure!(OpenFailure, Open);
semantic_failure!(TransportFailure, Transport);
semantic_failure!(DeliveryAckFailure, Delivery);
semantic_failure!(SubscriptionClosed, Subscription);
semantic_failure!(LifecycleOutcome, Lifecycle);
impl fmt::Display for OperationFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Preserve the closed refusal while withholding authenticated cursor
        // and takeover coordinates from operator diagnostics.
        let cause = match &self.cause {
            OperationCause::Subscription(SubscriptionClosed::DisplacedByTakeover { .. }) => {
                "Subscription(DisplacedByTakeover)".to_owned()
            }
            OperationCause::Transport(TransportFailure::CursorInvalid(invalid)) => {
                format!("Transport(CursorInvalid({}))", invalid.reason().as_str())
            }
            cause => format!("{cause:?}"),
        };
        let diagnostic = crate::BoundedDiagnostic::local(format!(
            "remote mail operation refused: {cause}; effect {:?}; source {:?}",
            self.effect, self.source_state
        ));
        f.write_str(diagnostic.as_str())
    }
}
impl std::error::Error for OperationFailure {}

#[cfg(test)]
mod tests;
