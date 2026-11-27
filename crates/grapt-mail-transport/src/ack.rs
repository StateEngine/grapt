//! Delivery/admission acknowledgement — the fact that is NOT reading.
//!
//! # Why this exists at all
//!
//! StateEngine's `msg_ack` means READ: it appends a read marker and puts the
//! acker into `read_by`, telling the sender a human-or-agent attended to the
//! message. That is a completely different fact from "this exact message became
//! durable in the recipient's local mailbox". A transport that used `msg_ack` to
//! settle its cursor would report attention that never happened, and would do so
//! at exactly the moment the recipient is least able to notice — during
//! automatic catch-up.
//!
//! So delivery acknowledgement records only: this exact source message was
//! durably admitted for this exact verified recipient under this exact protocol
//! binding. It must not mark read or handled, answer a question, resolve a
//! blocker, archive, dismiss or supersede the row, close a queue item, or
//! acknowledge human attention.
//!
//! # Crash-safe order (fixed by the admission packet, unchanged by J1-J8)
//!
//! ```text
//! remote fetch
//!   -> verify binding and strict complete DTO
//!   -> local governed admission
//!   -> durably record local delivery-ack debt
//!   -> remote delivery/admission acknowledgement
//!   -> atomically settle local debt and advance the local source checkpoint
//!   -> later, independently, handler success then local consumed marker
//!   -> separately, if ever commissioned, read/handled acknowledgement
//! ```
//!
//! A lost response leaves DEBT. Restart lists the debt and retries the exact
//! acknowledgement; it never deletes debt, advances a checkpoint, or marks a
//! message handled because the transport was uncertain. The debt row and the
//! checkpoint transaction belong to R3b-2B, not to this crate.
//!
//! # Residual
//!
//! The addendum's amendment 7 leaves the MINIMAL remote coordinate set to the
//! ratified provider contract: StateEngine should not be required to store SE
//! Code-internal fields that do not contribute to remote idempotency. The set
//! below is this workspace's complete proposal, and the local half — receipt,
//! digest, admission identity — is bound locally regardless of what the provider
//! ends up persisting.

use crate::identity::{DeliveryAdmissionAckId, ProjectPair, RemoteMailboxMessageId};

pub use grapt_types::remote_mail::ack::{DeliveryAdmissionAck, DeliveryAdmissionAckParts};

pub use grapt_types::remote_mail::SecondaryProjectionState;

/// The provider's immutable record of one delivery acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryAckReceipt {
    projection: Option<SecondaryProjectionState>,
    ack_id: DeliveryAdmissionAckId,
    source_message: RemoteMailboxMessageId,
    projects: ProjectPair,
}

impl DeliveryAckReceipt {
    pub fn new(
        ack_id: DeliveryAdmissionAckId,
        source_message: RemoteMailboxMessageId,
        projects: ProjectPair,
    ) -> Self {
        DeliveryAckReceipt {
            projection: None,
            ack_id,
            source_message,
            projects,
        }
    }

    pub(crate) fn with_projection(mut self, state: SecondaryProjectionState) -> Self {
        self.projection = Some(state);
        self
    }
    pub fn projection(&self) -> Option<SecondaryProjectionState> {
        self.projection
    }
    pub fn ack_id(&self) -> &DeliveryAdmissionAckId {
        &self.ack_id
    }

    pub fn source_message(&self) -> &RemoteMailboxMessageId {
        &self.source_message
    }

    pub fn projects(&self) -> &ProjectPair {
        &self.projects
    }
}

/// What an acknowledgement did.
///
/// Both variants are success. `ExactRetry` returns the ORIGINAL receipt, not a
/// new one — a second identical acknowledgement must not create a second fact,
/// or a lost response would inflate the delivery record every time it was
/// retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryAckOutcome {
    Recorded(DeliveryAckReceipt),
    ExactRetry(DeliveryAckReceipt),
}

impl DeliveryAckOutcome {
    pub fn receipt(&self) -> &DeliveryAckReceipt {
        match self {
            DeliveryAckOutcome::Recorded(receipt) | DeliveryAckOutcome::ExactRetry(receipt) => {
                receipt
            }
        }
    }

    /// True when this call created the record rather than reconciling with it.
    pub fn is_first_record(&self) -> bool {
        matches!(self, DeliveryAckOutcome::Recorded(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{
        ExclusiveSourceCursor, MailboxMessageDigest, SourceBindingId, SourceProjectId,
        TargetProjectId,
    };
    use grapt_types::mailbox::{MailboxAdmissionSeq, MailboxMessageId, SeatRecipientId};
    use grapt_types::protocol::ProtocolVersion;

    fn projects() -> Result<ProjectPair, Box<dyn std::error::Error>> {
        Ok({
            ProjectPair::new(
                SourceProjectId::new("stateengine").ok_or("source")?,
                TargetProjectId::new("secode").ok_or("target")?,
            )
        })
    }

    fn ack(
        source_message: &str,
        digest: &str,
    ) -> Result<DeliveryAdmissionAck, Box<dyn std::error::Error>> {
        Ok({
            DeliveryAdmissionAck::new(DeliveryAdmissionAckParts {
                ack_id: DeliveryAdmissionAckId::new("ack-1").ok_or("ack")?,
                source_binding: SourceBindingId::new("bind-1").ok_or("binding")?,
                projects: projects()?,
                source_message: RemoteMailboxMessageId::new(source_message).ok_or("message")?,
                source_cursor: ExclusiveSourceCursor::new("cur-1").ok_or("cursor")?,
                recipient: SeatRecipientId::new("seat-delta").ok_or("recipient")?,
                local_message: MailboxMessageId::new("msg-local-1").ok_or("local")?,
                local_admission: MailboxAdmissionSeq::FIRST,
                canonical_message_digest: MailboxMessageDigest::new(digest).ok_or("digest")?,
                protocol: ProtocolVersion::new(crate::REMOTE_SEAT_MAIL_TRANSPORT_V1)
                    .ok_or("protocol")?,
            })
        })
    }

    #[test]
    fn an_exact_repeat_is_recognised_as_the_same_acknowledgement(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let first = ack("msg-remote-1", "digest-a")?;
        let repeat = ack("msg-remote-1", "digest-a")?;
        assert!(first.is_exact_repeat_of(&repeat));
        Ok(())
    }

    #[test]
    fn a_different_digest_or_message_is_not_an_exact_repeat(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let first = ack("msg-remote-1", "digest-a")?;
        // Same message, different content digest: the source changed under us,
        // which is a conflict to refuse, never a retry to absorb.
        assert!(!first.is_exact_repeat_of(&ack("msg-remote-1", "digest-b")?));
        // Same digest, different message: coordinates must match too.
        assert!(!first.is_exact_repeat_of(&ack("msg-remote-2", "digest-a")?));
        Ok(())
    }

    #[test]
    fn an_exact_retry_returns_the_original_receipt_and_creates_no_second_fact(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let receipt = DeliveryAckReceipt::new(
            DeliveryAdmissionAckId::new("ack-1").ok_or("ack")?,
            RemoteMailboxMessageId::new("msg-remote-1").ok_or("message")?,
            projects()?,
        );
        let recorded = DeliveryAckOutcome::Recorded(receipt.clone());
        let retried = DeliveryAckOutcome::ExactRetry(receipt.clone());

        assert!(recorded.is_first_record());
        assert!(!retried.is_first_record());
        // The identity a lost response would otherwise duplicate.
        assert_eq!(recorded.receipt(), retried.receipt());
        assert_eq!(retried.receipt().ack_id().as_str(), "ack-1");
        Ok(())
    }

    #[test]
    fn the_acknowledgement_carries_both_project_halves() -> Result<(), Box<dyn std::error::Error>> {
        let ack = ack("msg-remote-1", "digest-a")?;
        assert_eq!(ack.projects().source().as_str(), "stateengine");
        assert_eq!(ack.projects().target().as_str(), "secode");
        Ok(())
    }

    #[test]
    fn an_acknowledgement_is_identified_only_by_delivery_coordinates(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The separation from reading is structural: every coordinate this type
        // carries answers "which message became durable where", and none of them
        // can express attention. A `read` accessor does not exist to be called —
        // that absence is enforced by the compile-fail probe in
        // `tests/probes/`, not by inspecting a rendered string. An earlier
        // version of this test asserted `!format!("{ack:?}").contains("read")`,
        // which is a witness that can pass for the wrong reason: the sibling
        // dismiss test using the same trick matched "read" inside "AlreadyHandled".
        let ack = ack("msg-remote-1", "digest-a")?;
        assert_eq!(ack.source_message().as_str(), "msg-remote-1");
        assert_eq!(ack.source_cursor().as_str(), "cur-1");
        assert_eq!(ack.local_message().as_str(), "msg-local-1");
        assert_eq!(ack.local_admission(), MailboxAdmissionSeq::FIRST);
        assert_eq!(ack.canonical_message_digest().as_str(), "digest-a");
        assert_eq!(ack.recipient().as_str(), "seat-delta");
        Ok(())
    }
}
