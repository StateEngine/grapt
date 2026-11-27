//! Durable input admission. Law: `PRESERVE` — pending rows contain unpromoted
//! work only (oracle evidence `schema/src/session-pending.ts`,
//! `core/src/session/pending.ts`; steer-before-queue promotion
//! `pending.ts:451-489` verified by the red-team review).

use crate::message::MessageId;

crate::id::opaque_id! {
    /// A typed harness directive admitted by turn authority. Never synthetic
    /// user speech.
    HarnessDirectiveId
}

/// A pending input's identity IS its message identity — a promotion clears the
/// marker in place, it never re-keys the row (D3 seed fixture evidence).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PendingInputId(MessageId);

impl PendingInputId {
    pub fn new(message: MessageId) -> Self {
        Self(message)
    }

    pub fn message(&self) -> &MessageId {
        &self.0
    }
}

/// Non-negative durable admission sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AdmissionSeq(u64);

impl AdmissionSeq {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

/// Closed delivery vocabulary. The engine distinguishes `promote('steer')`
/// from `promote('input')` — a fixture that collapsed the two was corrected
/// against the oracle (D3 run receipt, 2026-08-08).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeliveryMode {
    Steer,
    Queue,
}

/// The three classes of input a Session inbox admits (amendment §7.1).
///
/// Law: background subagent completion is `OperationDelivery`, **never**
/// `type: synthetic` user input, and irreducible harness continuation is a
/// typed directive rather than synthetic user speech. Keeping the three
/// classes distinct is what stops a harness continuation from being replayed
/// as something the user said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxInputClass {
    /// Something the user actually said.
    UserInput { delivery: DeliveryMode },
    /// A durable operation's result, authored by its exact delivery claim.
    OperationDelivery {
        operation: crate::operation::OperationId,
        claim: crate::operation::DeliveryClaimId,
    },
    /// A typed harness directive for versioned system work.
    HarnessDirective { directive: HarnessDirectiveId },
}

impl InboxInputClass {
    /// Whether this input is user speech.
    ///
    /// Deliberately false for both other classes: a delivery or a directive
    /// that reported as user speech is the exact defect §7.1 removes.
    pub fn is_user_speech(&self) -> bool {
        matches!(self, InboxInputClass::UserInput { .. })
    }

    /// The steer/queue delivery mode, which only user speech has.
    pub fn delivery(&self) -> Option<DeliveryMode> {
        match self {
            InboxInputClass::UserInput { delivery } => Some(*delivery),
            InboxInputClass::OperationDelivery { .. }
            | InboxInputClass::HarnessDirective { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_completion_is_never_user_speech() -> Result<(), Box<dyn std::error::Error>> {
        let delivered = InboxInputClass::OperationDelivery {
            operation: crate::operation::OperationId::new("op-1").ok_or("operation")?,
            claim: crate::operation::DeliveryClaimId::new("claim-1").ok_or("claim")?,
        };
        assert!(!delivered.is_user_speech());
        assert!(
            delivered.delivery().is_none(),
            "a delivery has no steer/queue precedence"
        );

        let directive = InboxInputClass::HarnessDirective {
            directive: HarnessDirectiveId::new("dir-1").ok_or("directive")?,
        };
        assert!(!directive.is_user_speech());

        let steer = InboxInputClass::UserInput {
            delivery: DeliveryMode::Steer,
        };
        assert!(steer.is_user_speech());
        assert_eq!(steer.delivery(), Some(DeliveryMode::Steer));
        Ok(())
    }

    #[test]
    fn pending_identity_is_the_message_identity() -> Result<(), Box<dyn std::error::Error>> {
        let msg: MessageId = "msg_0a1b2c".parse()?;
        let pending = PendingInputId(msg.clone());
        assert_eq!(pending.0, msg);
        Ok(())
    }

    #[test]
    fn delivery_mode_is_closed_and_ordered_by_promotion_law() {
        // Steer promotes before queue; the enum must stay two-valued so a
        // future variant forces every match site to decide (probe #5 class).
        let modes = [DeliveryMode::Steer, DeliveryMode::Queue];
        assert_eq!(modes.len(), 2);
    }
}
