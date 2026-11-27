//! Original delivery coordinates retained by the durable mailbox.

use super::{
    DeliveryAdmissionAckId, ExclusiveSourceCursor, MailboxMessageDigest, ProjectPair,
    RemoteMailboxMessageId, SourceBindingId,
};
use crate::mailbox::{MailboxAdmissionSeq, MailboxMessageId, SeatRecipientId};
use crate::protocol::ProtocolVersion;

/// One delivery/admission acknowledgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryAdmissionAck {
    ack_id: DeliveryAdmissionAckId,
    source_binding: SourceBindingId,
    projects: ProjectPair,
    source_message: RemoteMailboxMessageId,
    source_cursor: ExclusiveSourceCursor,
    recipient: SeatRecipientId,
    local_message: MailboxMessageId,
    local_admission: MailboxAdmissionSeq,
    canonical_message_digest: MailboxMessageDigest,
    protocol: ProtocolVersion,
}

/// The parts an acknowledgement is built from.
#[derive(Debug, Clone)]
pub struct DeliveryAdmissionAckParts {
    pub ack_id: DeliveryAdmissionAckId,
    pub source_binding: SourceBindingId,
    pub projects: ProjectPair,
    pub source_message: RemoteMailboxMessageId,
    pub source_cursor: ExclusiveSourceCursor,
    pub recipient: SeatRecipientId,
    pub local_message: MailboxMessageId,
    pub local_admission: MailboxAdmissionSeq,
    pub canonical_message_digest: MailboxMessageDigest,
    pub protocol: ProtocolVersion,
}

impl DeliveryAdmissionAck {
    pub fn new(parts: DeliveryAdmissionAckParts) -> Self {
        DeliveryAdmissionAck {
            ack_id: parts.ack_id,
            source_binding: parts.source_binding,
            projects: parts.projects,
            source_message: parts.source_message,
            source_cursor: parts.source_cursor,
            recipient: parts.recipient,
            local_message: parts.local_message,
            local_admission: parts.local_admission,
            canonical_message_digest: parts.canonical_message_digest,
            protocol: parts.protocol,
        }
    }

    pub fn ack_id(&self) -> &DeliveryAdmissionAckId {
        &self.ack_id
    }

    pub fn source_binding(&self) -> &SourceBindingId {
        &self.source_binding
    }

    pub fn projects(&self) -> &ProjectPair {
        &self.projects
    }

    pub fn source_message(&self) -> &RemoteMailboxMessageId {
        &self.source_message
    }

    pub fn source_cursor(&self) -> &ExclusiveSourceCursor {
        &self.source_cursor
    }

    pub fn recipient(&self) -> &SeatRecipientId {
        &self.recipient
    }

    pub fn local_message(&self) -> &MailboxMessageId {
        &self.local_message
    }

    pub fn local_admission(&self) -> MailboxAdmissionSeq {
        self.local_admission
    }

    pub fn canonical_message_digest(&self) -> &MailboxMessageDigest {
        &self.canonical_message_digest
    }

    pub fn protocol(&self) -> &ProtocolVersion {
        &self.protocol
    }

    /// Whether two acknowledgements are the SAME acknowledgement.
    ///
    /// Every coordinate and the digest must match. This is what makes a retry
    /// after a lost response safe: an exact repeat reconciles to the original
    /// immutable receipt, while anything else is a conflict rather than a
    /// second opinion.
    pub fn is_exact_repeat_of(&self, other: &DeliveryAdmissionAck) -> bool {
        self == other
    }
}
