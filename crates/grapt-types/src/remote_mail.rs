//! Opaque transport identities.
//!
//! Every identity here is bounded, non-empty and opaque. "Opaque" is load
//! bearing: Grapt never parses, orders or does arithmetic on a value the
//! SOURCE minted. The reconciliation addendum records why — StateEngine's
//! current continuation is a caller-visible byte offset, and wrapping arithmetic
//! around it would make a private mechanism into a contract this workspace
//! cannot honour.
//!
//! The ratified transport identities are shared with durable mailbox receipts.
//! This foundational module owns validation only: no binding, credential,
//! codec, registry, scheduling or provider implementation lives here.
//! Mail transport reexports these same types; store/runtime never need its
//! HTTP/TLS dependencies to name durable coordinates.

pub mod ack;
pub mod progress;

use core::fmt;

/// The bound every opaque transport identity is measured against, in UTF-8
/// bytes. One shared bound, so no identity acquires a private, larger one.
pub const MAX_TRANSPORT_IDENTITY_BYTES: usize = 256;

/// Why an opaque transport identity could not be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// Identity text is absent. Absence is never a wildcard.
    Empty { type_name: &'static str },
    /// Identity text exceeds the shared byte bound. Refused, never truncated: a
    /// truncated identity would silently alias a different one.
    TooLarge {
        type_name: &'static str,
        bytes: usize,
        max: usize,
    },
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::Empty { type_name } => {
                write!(f, "{type_name} must not be empty")
            }
            IdentityError::TooLarge {
                type_name,
                bytes,
                max,
            } => write!(
                f,
                "{type_name} is {bytes} UTF-8 bytes, above the {max}-byte bound; \
                 it is refused, never truncated"
            ),
        }
    }
}

impl std::error::Error for IdentityError {}

macro_rules! opaque_transport_identity {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// The one validating constructor.
            pub fn try_new(raw: impl Into<String>) -> Result<Self, IdentityError> {
                let raw = raw.into();
                if raw.is_empty() {
                    return Err(IdentityError::Empty {
                        type_name: stringify!($name),
                    });
                }
                let bytes = raw.len();
                if bytes > MAX_TRANSPORT_IDENTITY_BYTES {
                    return Err(IdentityError::TooLarge {
                        type_name: stringify!($name),
                        bytes,
                        max: MAX_TRANSPORT_IDENTITY_BYTES,
                    });
                }
                Ok(Self(raw))
            }

            /// Convenience route. Validation stays in [`Self::try_new`], so this
            /// cannot bypass the bound.
            pub fn new(raw: impl Into<String>) -> Option<Self> {
                Self::try_new(raw).ok()
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

opaque_transport_identity! {
    /// The foreign project a message originated in.
    ///
    /// Ruling J8: present on every authority-bearing object EVEN WHEN it equals
    /// the target. The addendum's Q-5D-12 finding is the reason — same-project
    /// DTOs currently omit it, and an omitted origin cannot be distinguished
    /// from an erased one by a cross-project adapter.
    SourceProjectId
}

opaque_transport_identity! {
    /// The project a message was delivered INTO. Always present, always paired
    /// with [`SourceProjectId`].
    TargetProjectId
}

opaque_transport_identity! {
    /// One negotiated transport binding between this workspace and one foreign
    /// project/principal/recipient triple.
    ///
    /// Not a lease. Ruling J2 removed leases and fences outright; this names
    /// WHICH binding a value belongs to, and never claims exclusive ownership.
    SourceBindingId
}

opaque_transport_identity! {
    /// A message's immutable identity as the SOURCE minted it.
    ///
    /// Distinct from `grapt_types::mailbox::MailboxMessageId`, which is the
    /// local durable identity. The two are related by admission, never equal by
    /// assumption: exact-retry reconciliation compares source identity against
    /// source identity.
    RemoteMailboxMessageId
}

opaque_transport_identity! {
    /// The authenticated foreign principal a message came from.
    ///
    /// Obtained from the adapter's configured transport binding, never from a
    /// caller-supplied field and never from the message's own envelope.
    SourcePrincipalId
}

opaque_transport_identity! {
    /// The source's EXCLUSIVE position token.
    ///
    /// A page request means strictly after this value. It is opaque, bounded and
    /// integrity-protected by its issuer; this workspace stores it and returns
    /// it, and does nothing else with it. In particular it is never compared
    /// with, derived from or substituted for the local `MailboxCursor`.
    ExclusiveSourceCursor
}

opaque_transport_identity! {
    /// The source's declared creation time, retained verbatim.
    ///
    /// Deliberately opaque rather than a number: enumeration order is the
    /// cursor's job, and a timestamp that could be compared would invite an
    /// ordering that the source never promised. It is provenance, not sequence.
    SourceTimestamp
}

opaque_transport_identity! {
    /// A stable snapshot's identity.
    MailSnapshotId
}

opaque_transport_identity! {
    /// The linearization point a snapshot and its armed subscription share.
    ///
    /// Ruling J3: one atomic operation returns both, so this value is the single
    /// proof that no mutation falls between "what the snapshot contains" and
    /// "what the subscription covers".
    SnapshotBarrier
}

opaque_transport_identity! {
    /// The origin a safe, no-skip replay may start from after invalidation.
    ///
    /// Ruling J4: on invalidation the provider replays from the earliest
    /// retained origin guaranteed to COVER the checkpoint. If no such origin
    /// exists the provider must say so; it may never substitute the head.
    SafeReplayOrigin
}

opaque_transport_identity! {
    /// One armed subscription's identity.
    MailSubscriptionId
}

opaque_transport_identity! {
    /// The local process instance that opened a binding.
    MailTransportClientInstanceId
}

opaque_transport_identity! {
    /// The version of the recipient mapping the provider resolved with.
    RecipientMappingVersion
}

opaque_transport_identity! {
    /// The concrete adapter implementation's version.
    AdapterVersion
}

opaque_transport_identity! {
    /// The canonical digest of one locally admitted message, bound into the
    /// delivery acknowledgement so an exact retry can be proven exact.
    MailboxMessageDigest
}

opaque_transport_identity! {
    /// One delivery/admission acknowledgement's identity.
    DeliveryAdmissionAckId
}

/// The project pair carried by every authority-bearing object (ruling J8).
///
/// A pair rather than two loose fields, so no constructor can accept one and
/// forget the other. Equality of the two halves is legal and explicitly
/// preserved — same-project delivery still records both.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectPair {
    source: SourceProjectId,
    target: TargetProjectId,
}

impl ProjectPair {
    pub fn new(source: SourceProjectId, target: TargetProjectId) -> Self {
        ProjectPair { source, target }
    }

    pub fn source(&self) -> &SourceProjectId {
        &self.source
    }

    pub fn target(&self) -> &TargetProjectId {
        &self.target
    }

    /// True when a message did not cross a project boundary.
    ///
    /// Reported, never used to drop a field: J8's whole point is that the pair
    /// survives even when both halves agree.
    pub fn is_same_project(&self) -> bool {
        self.source.as_str() == self.target.as_str()
    }
}

/// StateEngine's exact-principal action classification. Kind/prose is not authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailAttention {
    Wake,
    Quiet,
}
impl MailAttention {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "wake" => Some(Self::Wake),
            "quiet" => Some(Self::Quiet),
            _ => None,
        }
    }
}

/// Why the source archived a message.
///
/// Closed. An unknown reason is a decode failure, not an `Other(String)`:
/// archive provenance feeds a human's understanding of why mail vanished from a
/// live view, and a passthrough string would make that unauditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceArchiveReason {
    /// A later message explicitly superseded this one.
    ExplicitSupersession,
    /// The recipient dismissed it — declined to ingest, which is NOT reading it.
    RecipientDismissed,
    /// The source's own retention policy aged it out.
    SourceRetention,
    /// An operator acted on it directly.
    Administrative,
}

impl SourceArchiveReason {
    pub const ALL: [SourceArchiveReason; 4] = [
        SourceArchiveReason::ExplicitSupersession,
        SourceArchiveReason::RecipientDismissed,
        SourceArchiveReason::SourceRetention,
        SourceArchiveReason::Administrative,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            SourceArchiveReason::ExplicitSupersession => "explicit-supersession",
            SourceArchiveReason::RecipientDismissed => "recipient-dismissed",
            SourceArchiveReason::SourceRetention => "source-retention",
            SourceArchiveReason::Administrative => "administrative",
        }
    }

    /// Fails closed.
    pub fn parse(raw: &str) -> Option<Self> {
        SourceArchiveReason::ALL
            .into_iter()
            .find(|reason| reason.as_str() == raw)
    }
}

/// Whether the SOURCE considers a message archived.
///
/// This is source provenance. It is not a local TTL and it is not an
/// instruction to mark the local row handled. If the source cannot state the
/// relation and reason explicitly, the capability is incompatible — omission is
/// never interpreted as `Active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceArchiveDisposition {
    Active,
    Archived(SourceArchiveReason),
}

/// Secondary projection is evidence separate from witnessed primary delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecondaryProjectionState {
    Posted,
    AlreadyPresent,
    Unarmed,
    Refused,
    Unreadable,
}

opaque_transport_identity! {
    /// One local receiving relationship and explicit protocol context.
    MailStreamId
}
