//! `remote-seat-mail-transport/v1` — the sealed Grapt side of a remote seat
//! mailbox.
//!
//! # What this crate is
//!
//! A project-local PORT plus its strict DTOs, closed failures, negotiated
//! bounds and a deterministic fake. It lets one verified Grapt recipient
//! converge from a foreign durable mail source without importing that source's
//! implementation or authority model.
//!
//! # Released adapter
//!
//! The STATEENGINE-NATIVE-01 real-integration admission releases the
//! crate-owned authenticated HTTP adapter against the versioned StateEngine
//! door. The deterministic fake remains for offline vectors. The old J7
//! fake-only restriction is historical; no wrapper around `msg_*` is used.
//! Delivery acknowledgement remains distinct from reading and dismissal.
//! The mandatory attention correction refuses old or unknown message shapes.
//!
//! # Ownership boundary
//!
//! This crate is R3b-2A. It owns no store schema (R3b-2B), no runtime loop or
//! reconnect owner (R3b-2C), and no composition/compatibility proof (R3b-2I).
//! It depends downward on identity vocabulary and existing HTTP/TLS packages,
//! with no store/runtime dependency or foreign implementation import.

pub mod ack;
pub mod binding;
pub mod cursor;
pub mod failure;
pub mod fake;
pub mod http;
pub mod identity;
pub mod limits;
pub mod message;
pub mod operation;
pub mod port;
pub mod signal;

#[cfg(test)]
pub(crate) mod test_support;

pub use ack::{
    DeliveryAckOutcome, DeliveryAckReceipt, DeliveryAdmissionAck, DeliveryAdmissionAckParts,
};
pub use fake::ScriptedBinding;
pub use port::{
    PageLimits, RemoteSeatMailTransportV1 as RemoteSeatMailTransportV1Port, ResumeRequest,
    ShutdownBudget, ShutdownReceipt,
};
pub use signal::{
    BulkDismissOutcome, BulkDismissRequest, ChangeSignal, DismissError, DismissReason,
    MAX_CHANGE_TOKEN_BYTES,
};

pub use binding::{
    BindingGeneration, DisplacedOwner, MailBinding, MailBindingReceipt, OpenRequest,
    TakeoverReceipt,
};
pub use cursor::{
    ArmedMailboxSubscription, CursorInvalidReason, CursorInvalidation, CursorRecovery,
    MailSnapshot, RemoteMailPage, SubscriptionGeneration,
};
pub use failure::{
    BoundedDiagnostic, DeliveryAckFailure, LifecycleOutcome, OpenFailure, SubscriptionClosed,
    TransportFailure,
};

pub use identity::{
    AdapterVersion, DeliveryAdmissionAckId, ExclusiveSourceCursor, IdentityError, MailSnapshotId,
    MailSubscriptionId, MailTransportClientInstanceId, MailboxMessageDigest, ProjectPair,
    RecipientMappingVersion, RemoteMailboxMessageId, SafeReplayOrigin, SnapshotBarrier,
    SourceBindingId, SourcePrincipalId, SourceProjectId, SourceTimestamp, TargetProjectId,
    MAX_TRANSPORT_IDENTITY_BYTES,
};
pub use limits::{
    ClampedFields, ProviderLimits, ProviderLimitsParts, SettledTransportLimits,
    GRAPT_MAX_ADVISORY_SIGNAL_PAYLOAD_BYTES, GRAPT_MAX_AGGREGATE_BODY_BYTES_PER_PAGE,
    GRAPT_MAX_BODY_BYTES, GRAPT_MAX_COALESCED_PENDING_SIGNALS, GRAPT_MAX_DIAGNOSTIC_BYTES,
    GRAPT_MAX_MESSAGES_PER_PAGE,
};
pub use message::{
    BoundRemoteSeatMailMessageParts, BoundRemoteSeatMailMessageV1, BoundedMailRef,
    BoundedOrderedMailRefs, MailAttention, MessageError, RemoteSeatMailKind,
    SourceArchiveDisposition, SourceArchiveReason, SourceSupersession,
};

/// The exact protocol identity this crate implements.
///
/// One literal, checked by the port: a binding that negotiated a different
/// version is refused rather than adapted, because "adapting" an unknown
/// protocol means guessing which of its fields are authoritative.
pub const REMOTE_SEAT_MAIL_TRANSPORT_V1: &str =
    grapt_types::remote_mail::progress::MailTransportVersion::V1.as_str();

/// Explicit transport selection; message DTOs and delivery ACKs remain v1.
pub use grapt_types::remote_mail::progress::{
    MailDecodeFailure, MailTransportVersion, RefusedIndexId, RefusedMailRow, RefusedRowReason,
};
pub const REMOTE_SEAT_MAIL_TRANSPORT_V2: &str =
    grapt_types::remote_mail::progress::MailTransportVersion::V2.as_str();
