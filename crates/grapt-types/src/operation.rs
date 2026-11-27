//! Durable operation law. `Operation` is the shared LAW, not a shared
//! implementation: Permission, Form, subagent, and shell each keep typed
//! payloads and their own application service; the common record exists
//! because they all need identity, owner route, terminality, delivery and
//! recovery (contract §"Durable operation lifecycle").

crate::id::opaque_id! {
    /// Validated stable operation identity — distinct from any child
    /// SessionId (`CORRECT`: the current subagent table keys operation by
    /// child session ID; that impersonation is unrepresentable here).
    OperationId
}

crate::id::opaque_id! {
    /// Immutable owner route of an operation.
    OperationRouteId
}

crate::id::opaque_id! {
    /// Opaque delivery claim. A claim is INTENT; delivery proof is a separate
    /// durable fact.
    DeliveryClaimId
}

/// Closed at first release: adding a kind breaks every owner match (`CORRECT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationKind {
    Permission,
    Form,
    Subagent,
    Shell,
}

/// Evidence carried by an externally-ambiguous operation (a dead runtime with
/// a possibly-live external effect). Opaque here; interpretation is owner law.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmbiguityEvidence(String);

impl AmbiguityEvidence {
    /// Ambiguity must say what was observed. Empty evidence is not evidence,
    /// so it is refused rather than stored as a blank explanation.
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            return None;
        }
        Some(Self(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Tagged operation state. No generic JSON `status`, no optional-field bag:
/// each variant carries exactly its required payload. `T` is the kind-owned
/// domain terminal payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationState<T> {
    Admitted,
    Running,
    /// Domain-terminal does NOT imply delivered.
    DomainTerminal(T),
    /// Never resumed; reconciliation must move this to `DomainTerminal`.
    ExternallyAmbiguous(AmbiguityEvidence),
}

/// Delivery is its own durable fact, separate from domain terminality.
/// A lost claim reopens only after proving the original route did not record
/// delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryState {
    Unclaimed,
    Delivering(DeliveryClaimId),
    Delivered,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ShellTerminal {
        Exited(i32),
    }

    #[test]
    fn domain_terminal_does_not_imply_delivered() {
        let op: OperationState<ShellTerminal> =
            OperationState::DomainTerminal(ShellTerminal::Exited(0));
        let delivery = DeliveryState::Unclaimed;
        // Both coexist: terminality and delivery are independent facts.
        assert!(matches!(op, OperationState::DomainTerminal(_)));
        assert_eq!(delivery, DeliveryState::Unclaimed);
    }

    #[test]
    fn operation_kinds_are_closed() {
        let kinds = [
            OperationKind::Permission,
            OperationKind::Form,
            OperationKind::Subagent,
            OperationKind::Shell,
        ];
        assert_eq!(kinds.len(), 4);
    }
}
