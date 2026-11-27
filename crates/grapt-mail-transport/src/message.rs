//! The strict `remote-seat-mail-transport/v1` message DTO.
//!
//! Ruling J5 fixes this shape: source and target project, binding/protocol/
//! cursor, immutable message id, timestamp, exact sender and recipient, the
//! kind (known, or a verbatim label for a newer kind), a bounded body, canonical ordered refs, at most one direct
//! `supersedes` and one `superseded_by` edge, and one closed archive
//! disposition with a closed reason.
//!
//! # What is deliberately absent
//!
//! Read and handled state. They are not fields of this type and cannot filter
//! v1 enumeration. `msg_ack` means READ, and the whole reason this contract
//! exists is that delivery is a different fact from attention — a DTO that
//! carried read state would let a reader's inattention look like undelivered
//! mail, or worse, let delivery look like it had been read.

use crate::identity::{
    ExclusiveSourceCursor, IdentityError, ProjectPair, RemoteMailboxMessageId, SourceBindingId,
    SourcePrincipalId, SourceTimestamp,
};
use crate::limits::SettledTransportLimits;
use core::fmt;
use grapt_types::mailbox::{MailKindLabel, MailboxBody, SeatRecipientId};
use grapt_types::protocol::ProtocolVersion;

/// The kinds a seat mailbox message may be: the known set, or a verbatim label.
///
/// Ruling and RulingRequest remain distinct: the landed
/// R3a local vocabulary (`grapt_types::mailbox::SeatMessageKind`) had six and
/// omitted `Ruling`, so a settled ruling arriving over the wire had nowhere to
/// decode to. The admission packet is explicit that `Ruling` may not be
/// defaulted, dropped, or mapped onto `RulingRequest` — a request for a decision
/// and the decision itself are opposite facts, and collapsing them would let a
/// seat read "someone asked" where the floor said "it is settled".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RemoteSeatMailKind {
    /// Points at work to do.
    TaskPointer,
    /// Asks something that needs an answer before work continues.
    Question,
    /// Reports an obstruction on an exact path.
    Blocker,
    /// Reports completed work.
    Done,
    /// Transfers ownership of work.
    Handoff,
    /// Reports an observation for the recipient to assess.
    Finding,
    /// Asks an authority to settle a material ambiguity.
    RulingRequest,
    /// Carries a settled decision from that authority.
    Ruling,
    /// A kind the source floor added that this build does not know. It is
    /// mail all the same: admitted, listed and readable with its label as is.
    Other(MailKindLabel),
}

impl RemoteSeatMailKind {
    /// Every known kind. [`Self::parse`] consults this list first, so a known
    /// variant missing here would decode as [`Self::Other`].
    pub const KNOWN: [RemoteSeatMailKind; 8] = [
        RemoteSeatMailKind::TaskPointer,
        RemoteSeatMailKind::Question,
        RemoteSeatMailKind::Blocker,
        RemoteSeatMailKind::Done,
        RemoteSeatMailKind::Handoff,
        RemoteSeatMailKind::Finding,
        RemoteSeatMailKind::RulingRequest,
        RemoteSeatMailKind::Ruling,
    ];

    pub fn as_str(&self) -> &str {
        match self {
            RemoteSeatMailKind::TaskPointer => "task-pointer",
            RemoteSeatMailKind::Question => "question",
            RemoteSeatMailKind::Blocker => "blocker",
            RemoteSeatMailKind::Done => "done",
            RemoteSeatMailKind::Handoff => "handoff",
            RemoteSeatMailKind::Finding => "finding",
            RemoteSeatMailKind::RulingRequest => "ruling-request",
            RemoteSeatMailKind::Ruling => "ruling",
            RemoteSeatMailKind::Other(label) => label.as_str(),
        }
    }

    /// A known label decodes to its own kind, never the nearest-looking
    /// neighbour; any other admissible label is [`Self::Other`]. `None` only
    /// for a label [`MailKindLabel`] refuses (empty, oversize, control bytes).
    pub fn parse(raw: &str) -> Option<Self> {
        RemoteSeatMailKind::KNOWN
            .into_iter()
            .find(|kind| kind.as_str() == raw)
            .or_else(|| MailKindLabel::new(raw).map(RemoteSeatMailKind::Other))
    }
}

pub use grapt_types::remote_mail::{MailAttention, SourceArchiveDisposition, SourceArchiveReason};

/// One reference token carried by a message, bounded in UTF-8 bytes.
///
/// The addendum's amendment 4 narrowed an earlier overclaim: this preserves the
/// source's CANONICAL PERSISTED value, not the raw author input. StateEngine
/// already trims a ref before storing it, so "every source ref byte" was never
/// obtainable and promising it would have made the contract unimplementable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BoundedMailRef(String);

impl BoundedMailRef {
    pub fn try_new(raw: impl Into<String>, max_bytes: usize) -> Result<Self, MessageError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(MessageError::EmptyRef);
        }
        let bytes = raw.len();
        if bytes > max_bytes {
            return Err(MessageError::ReferenceBytesExceeded {
                bytes,
                max: max_bytes,
            });
        }
        Ok(BoundedMailRef(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn len_bytes(&self) -> usize {
        self.0.len()
    }
}

/// A message's references, in source order.
///
/// Order is preserved because refs are positional evidence, not a set: the
/// first ref of a task pointer routinely names the base commit and the second
/// the plan. Sorting or de-duplicating them would silently rewrite meaning.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoundedOrderedMailRefs(Vec<BoundedMailRef>);

impl BoundedOrderedMailRefs {
    /// Validates count and aggregate byte bounds together.
    ///
    /// Both bounds are checked BEFORE the collection is retained, so an
    /// oversize ref list cannot exist as a value even briefly.
    pub fn try_new(
        refs: Vec<BoundedMailRef>,
        max_count: u32,
        max_aggregate_bytes: usize,
    ) -> Result<Self, MessageError> {
        let count = u32::try_from(refs.len()).unwrap_or(u32::MAX);
        if count > max_count {
            return Err(MessageError::ReferenceCountExceeded {
                count,
                max: max_count,
            });
        }
        let aggregate: usize = refs.iter().map(BoundedMailRef::len_bytes).sum();
        if aggregate > max_aggregate_bytes {
            return Err(MessageError::AggregateReferenceBytesExceeded {
                bytes: aggregate,
                max: max_aggregate_bytes,
            });
        }
        Ok(BoundedOrderedMailRefs(refs))
    }

    /// The empty list. Distinct from absence at the wire boundary: a message
    /// with no refs is a fact, and the decoder refuses a MISSING refs field
    /// rather than defaulting it here.
    pub fn empty() -> Self {
        BoundedOrderedMailRefs(Vec::new())
    }

    pub fn as_slice(&self) -> &[BoundedMailRef] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A message's supersession edges.
///
/// Ruling J5 changed this from the admission packet's either/or enum
/// (`None | Supersedes | SupersededBy`) to AT MOST ONE of each, independently.
/// The old shape could not represent the ordinary middle of a chain: a
/// correction that both replaces an earlier message and is itself replaced
/// later. Under the enum, one of those two true facts had to be discarded.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceSupersession {
    supersedes: Option<RemoteMailboxMessageId>,
    superseded_by: Option<RemoteMailboxMessageId>,
}

impl SourceSupersession {
    /// No edges in either direction.
    pub fn none() -> Self {
        SourceSupersession::default()
    }

    /// Validates both edges against the message they belong to.
    ///
    /// A self-edge in either direction is a decode failure, and so is an edge
    /// that names the same neighbour in both directions — that pair asserts A
    /// replaced B and B replaced A, which no ordering can satisfy.
    pub fn try_new(
        message: &RemoteMailboxMessageId,
        supersedes: Option<RemoteMailboxMessageId>,
        superseded_by: Option<RemoteMailboxMessageId>,
    ) -> Result<Self, MessageError> {
        if supersedes.as_ref() == Some(message) || superseded_by.as_ref() == Some(message) {
            return Err(MessageError::SupersessionSelfReference {
                message: message.as_str().to_owned(),
            });
        }
        if let (Some(back), Some(forward)) = (supersedes.as_ref(), superseded_by.as_ref()) {
            if back == forward {
                return Err(MessageError::SupersessionCycle {
                    message: message.as_str().to_owned(),
                    neighbour: back.as_str().to_owned(),
                });
            }
        }
        Ok(SourceSupersession {
            supersedes,
            superseded_by,
        })
    }

    pub fn supersedes(&self) -> Option<&RemoteMailboxMessageId> {
        self.supersedes.as_ref()
    }

    pub fn superseded_by(&self) -> Option<&RemoteMailboxMessageId> {
        self.superseded_by.as_ref()
    }
}

/// Why a message could not be admitted as a transport value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageError {
    EmptyRef,
    ReferenceBytesExceeded {
        bytes: usize,
        max: usize,
    },
    ReferenceCountExceeded {
        count: u32,
        max: u32,
    },
    AggregateReferenceBytesExceeded {
        bytes: usize,
        max: usize,
    },
    MessageBodyExceeded {
        bytes: usize,
        max: usize,
    },
    SupersessionSelfReference {
        message: String,
    },
    SupersessionCycle {
        message: String,
        neighbour: String,
    },
    /// The sender and recipient addresses are the same seat.
    SenderIsRecipient {
        address: String,
    },
    Identity(IdentityError),
}

impl From<IdentityError> for MessageError {
    fn from(value: IdentityError) -> Self {
        MessageError::Identity(value)
    }
}

impl fmt::Display for MessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageError::EmptyRef => f.write_str("a reference token must not be empty"),
            MessageError::ReferenceBytesExceeded { bytes, max } => write!(
                f,
                "a reference is {bytes} UTF-8 bytes, above the negotiated {max}-byte bound"
            ),
            MessageError::ReferenceCountExceeded { count, max } => write!(
                f,
                "a message carries {count} references, above the negotiated bound of {max}"
            ),
            MessageError::AggregateReferenceBytesExceeded { bytes, max } => write!(
                f,
                "a message's references total {bytes} UTF-8 bytes, above the negotiated {max}-byte bound"
            ),
            MessageError::MessageBodyExceeded { bytes, max } => write!(
                f,
                "a message body is {bytes} UTF-8 bytes, above the settled {max}-byte bound; \
                 it is refused, never truncated"
            ),
            MessageError::SupersessionSelfReference { message } => write!(
                f,
                "message {message:?} cannot supersede or be superseded by itself"
            ),
            MessageError::SupersessionCycle { message, neighbour } => write!(
                f,
                "message {message:?} claims {neighbour:?} both supersedes it and is superseded by it"
            ),
            MessageError::SenderIsRecipient { address } => {
                write!(f, "a seat cannot mail itself at {address:?}")
            }
            MessageError::Identity(inner) => write!(f, "{inner}"),
        }
    }
}

impl std::error::Error for MessageError {}

/// One strictly decoded remote message, ready to cross into local admission.
///
/// Every field is private and the only route in is [`Self::try_new`], so a
/// decoder cannot assemble one out of a raw transport map and no downstream
/// crate can widen it by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundRemoteSeatMailMessageV1 {
    protocol: ProtocolVersion,
    source_binding: SourceBindingId,
    projects: ProjectPair,
    source_message: RemoteMailboxMessageId,
    source_cursor: ExclusiveSourceCursor,
    sender: SourcePrincipalId,
    recipient: SeatRecipientId,
    kind: RemoteSeatMailKind,
    body: MailboxBody,
    refs: BoundedOrderedMailRefs,
    supersession: SourceSupersession,
    archive: SourceArchiveDisposition,
    source_created: SourceTimestamp,
    attention: MailAttention,
}

/// The parts a decoder supplies. A struct rather than thirteen positional
/// arguments, so a field cannot be transposed with its neighbour of the same
/// type at a call site.
#[derive(Debug, Clone)]
pub struct BoundRemoteSeatMailMessageParts {
    pub protocol: ProtocolVersion,
    pub source_binding: SourceBindingId,
    pub projects: ProjectPair,
    pub source_message: RemoteMailboxMessageId,
    pub source_cursor: ExclusiveSourceCursor,
    pub sender: SourcePrincipalId,
    pub recipient: SeatRecipientId,
    pub kind: RemoteSeatMailKind,
    pub body: MailboxBody,
    pub refs: BoundedOrderedMailRefs,
    pub supersession: SourceSupersession,
    pub archive: SourceArchiveDisposition,
    pub source_created: SourceTimestamp,
    pub attention: MailAttention,
}

impl BoundRemoteSeatMailMessageV1 {
    /// The one constructor.
    ///
    /// Re-checks the body against the SETTLED bound even though
    /// [`MailboxBody`] already enforces the Grapt ceiling: a provider may have
    /// negotiated a smaller profile, and the settled value is the one this
    /// binding promised to honour.
    pub fn try_new(
        parts: BoundRemoteSeatMailMessageParts,
        limits: &SettledTransportLimits,
    ) -> Result<Self, MessageError> {
        let bytes = parts.body.len_bytes();
        let max = limits.body_utf8_bytes();
        if bytes > max {
            return Err(MessageError::MessageBodyExceeded { bytes, max });
        }
        Ok(BoundRemoteSeatMailMessageV1 {
            protocol: parts.protocol,
            source_binding: parts.source_binding,
            projects: parts.projects,
            source_message: parts.source_message,
            source_cursor: parts.source_cursor,
            sender: parts.sender,
            recipient: parts.recipient,
            kind: parts.kind,
            body: parts.body,
            refs: parts.refs,
            supersession: parts.supersession,
            archive: parts.archive,
            source_created: parts.source_created,
            attention: parts.attention,
        })
    }

    pub fn protocol(&self) -> &ProtocolVersion {
        &self.protocol
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

    pub fn sender(&self) -> &SourcePrincipalId {
        &self.sender
    }

    pub fn recipient(&self) -> &SeatRecipientId {
        &self.recipient
    }

    pub fn kind(&self) -> &RemoteSeatMailKind {
        &self.kind
    }

    pub fn body(&self) -> &MailboxBody {
        &self.body
    }

    pub fn refs(&self) -> &BoundedOrderedMailRefs {
        &self.refs
    }

    pub fn supersession(&self) -> &SourceSupersession {
        &self.supersession
    }

    pub fn archive(&self) -> SourceArchiveDisposition {
        self.archive
    }

    pub fn attention(&self) -> MailAttention {
        self.attention
    }

    pub fn source_created(&self) -> &SourceTimestamp {
        &self.source_created
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::mailbox::SeatMessageKind;

    #[test]
    fn the_known_kinds_round_trip_and_other_labels_stay_verbatim() {
        assert_eq!(RemoteSeatMailKind::KNOWN.len(), 8);
        for kind in RemoteSeatMailKind::KNOWN {
            assert_eq!(RemoteSeatMailKind::parse(kind.as_str()), Some(kind));
        }
        let other = RemoteSeatMailKind::parse("ping");
        assert_eq!(other.as_ref().map(RemoteSeatMailKind::as_str), Some("ping"));
        assert!(matches!(other, Some(RemoteSeatMailKind::Other(_))));
        assert_eq!(RemoteSeatMailKind::parse(""), None);
        let labels: std::collections::BTreeSet<&str> = RemoteSeatMailKind::KNOWN
            .iter()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(labels.len(), RemoteSeatMailKind::KNOWN.len());
    }

    #[test]
    fn ruling_is_its_own_kind_and_is_never_a_ruling_request(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The named resurrection: mapping `ruling` onto `RulingRequest` would
        // turn a settled decision back into an open question.
        // Panics rather than returning `Err`. An `Err` return carries no
        // `panicked at` line, so the mutation harness records this row as
        // "failed without a captured diagnostic" and its evidence names no law
        // at all -- the row goes red for a reason nobody can read.
        let ruling = match RemoteSeatMailKind::parse("ruling") {
            Some(kind) => kind,
            None => panic!(
                "the wire label `ruling` must decode to its own kind: a settled \
                 Ruling may never be relabelled as a RulingRequest"
            ),
        };
        assert_eq!(ruling, RemoteSeatMailKind::Ruling);
        assert_ne!(ruling, RemoteSeatMailKind::RulingRequest);
        assert_ne!(
            RemoteSeatMailKind::Ruling.as_str(),
            RemoteSeatMailKind::RulingRequest.as_str()
        );
        Ok(())
    }

    #[test]
    fn the_transport_and_local_vocabularies_are_label_identical() {
        // One floor, two doors: a kind that decoded over the wire but had no
        // durable counterpart would be mail that arrives and can never be
        // admitted. The label sets must match exactly, in both directions.
        let remote: std::collections::BTreeSet<&str> = RemoteSeatMailKind::KNOWN
            .iter()
            .map(|k| k.as_str())
            .collect();
        let local: std::collections::BTreeSet<&str> =
            SeatMessageKind::KNOWN.iter().map(|k| k.as_str()).collect();
        assert_eq!(remote, local);
        assert_eq!(
            RemoteSeatMailKind::KNOWN.len(),
            SeatMessageKind::KNOWN.len()
        );
        // And the amendment is additive for every kind R3a already landed.
        for kind in SeatMessageKind::KNOWN {
            assert!(
                RemoteSeatMailKind::parse(kind.as_str()).is_some(),
                "local kind {} lost its transport counterpart",
                kind.as_str()
            );
        }
    }

    #[test]
    fn a_supersession_may_carry_both_edges_at_once() -> Result<(), Box<dyn std::error::Error>> {
        // The J5 amendment: the middle of a correction chain.
        let me = RemoteMailboxMessageId::try_new("msg-2")?;
        let earlier = RemoteMailboxMessageId::try_new("msg-1")?;
        let later = RemoteMailboxMessageId::try_new("msg-3")?;
        let edges = SourceSupersession::try_new(&me, Some(earlier), Some(later))?;
        assert_eq!(
            edges.supersedes().map(RemoteMailboxMessageId::as_str),
            Some("msg-1")
        );
        assert_eq!(
            edges.superseded_by().map(RemoteMailboxMessageId::as_str),
            Some("msg-3")
        );
        Ok(())
    }

    #[test]
    fn a_supersession_refuses_self_reference_in_either_direction(
    ) -> Result<(), crate::identity::IdentityError> {
        let me = RemoteMailboxMessageId::try_new("msg-2")?;
        let copy = RemoteMailboxMessageId::try_new("msg-2")?;
        assert!(matches!(
            SourceSupersession::try_new(&me, Some(copy.clone()), None),
            Err(MessageError::SupersessionSelfReference { .. })
        ));
        assert!(matches!(
            SourceSupersession::try_new(&me, None, Some(copy)),
            Err(MessageError::SupersessionSelfReference { .. })
        ));
        Ok(())
    }

    #[test]
    fn a_supersession_refuses_a_two_way_edge_to_one_neighbour(
    ) -> Result<(), crate::identity::IdentityError> {
        let me = RemoteMailboxMessageId::try_new("msg-2")?;
        let a = RemoteMailboxMessageId::try_new("msg-1")?;
        let b = RemoteMailboxMessageId::try_new("msg-1")?;
        assert!(matches!(
            SourceSupersession::try_new(&me, Some(a), Some(b)),
            Err(MessageError::SupersessionCycle { .. })
        ));
        Ok(())
    }

    #[test]
    fn refs_keep_their_source_order_and_are_not_deduplicated(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let refs = vec![
            BoundedMailRef::try_new("be1c33c2", 512)?,
            BoundedMailRef::try_new("3f9a607e", 512)?,
            BoundedMailRef::try_new("be1c33c2", 512)?,
        ];
        let ordered = BoundedOrderedMailRefs::try_new(refs, 64, 32_768)?;
        let seen: Vec<&str> = ordered
            .as_slice()
            .iter()
            .map(BoundedMailRef::as_str)
            .collect();
        assert_eq!(seen, ["be1c33c2", "3f9a607e", "be1c33c2"]);
        Ok(())
    }

    #[test]
    fn refs_refuse_count_and_aggregate_bounds_separately() -> Result<(), Box<dyn std::error::Error>>
    {
        let three = || -> Result<Vec<BoundedMailRef>, MessageError> {
            Ok(vec![
                BoundedMailRef::try_new("aaaa", 512)?,
                BoundedMailRef::try_new("bbbb", 512)?,
                BoundedMailRef::try_new("cccc", 512)?,
            ])
        };
        assert!(matches!(
            BoundedOrderedMailRefs::try_new(three()?, 2, 32_768),
            Err(MessageError::ReferenceCountExceeded { count: 3, max: 2 })
        ));
        assert!(matches!(
            BoundedOrderedMailRefs::try_new(three()?, 64, 8),
            Err(MessageError::AggregateReferenceBytesExceeded { bytes: 12, max: 8 })
        ));
        // Each bound alone is satisfiable; they are simultaneous, not either/or.
        assert!(BoundedOrderedMailRefs::try_new(three()?, 64, 32_768).is_ok());
        Ok(())
    }

    #[test]
    fn a_body_inside_the_ceiling_is_still_refused_above_the_settled_bound(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Ruling J6: the Grapt number is a CEILING and a provider may
        // negotiate a smaller measured profile. A body that fits 65,536 but
        // exceeds what this binding settled must be refused, or the transport
        // would fetch mail the binding promised it would not.
        // A deliberately smaller provider body cap.
        let settled = crate::test_support::provider_with(
            1_024,
            64,
            262_144,
            crate::limits::GRAPT_MAX_DIAGNOSTIC_BYTES,
        )
        .settle();
        assert_eq!(settled.body_utf8_bytes(), 1_024);

        // Inside the Grapt ceiling, outside this binding's settled bound.
        let oversize = "a".repeat(2_048);
        let body = MailboxBody::new(oversize)
            .map_err(|e| format!("the durable ceiling admits it: {e:?}"))?;
        let parts = BoundRemoteSeatMailMessageParts {
            protocol: ProtocolVersion::new("remote-seat-mail-transport/v1").ok_or("protocol")?,
            source_binding: SourceBindingId::new("bind-1").ok_or("binding")?,
            projects: ProjectPair::new(
                crate::identity::SourceProjectId::new("stateengine").ok_or("source")?,
                crate::identity::TargetProjectId::new("secode").ok_or("target")?,
            ),
            source_message: RemoteMailboxMessageId::new("msg-1").ok_or("message")?,
            source_cursor: ExclusiveSourceCursor::new("cur-1").ok_or("cursor")?,
            sender: SourcePrincipalId::new("agt-alpha").ok_or("sender")?,
            recipient: SeatRecipientId::new("seat-delta").ok_or("recipient")?,
            kind: RemoteSeatMailKind::TaskPointer,
            body,
            refs: BoundedOrderedMailRefs::empty(),
            supersession: SourceSupersession::none(),
            archive: SourceArchiveDisposition::Active,
            source_created: SourceTimestamp::new("2026-08-13T07:00:00Z").ok_or("timestamp")?,
            attention: crate::message::MailAttention::Wake,
        };

        match BoundRemoteSeatMailMessageV1::try_new(parts, &settled) {
            Err(MessageError::MessageBodyExceeded { bytes, max }) => {
                assert_eq!(bytes, 2_048);
                assert_eq!(max, 1_024);
            }
            other => panic!("a body above the settled bound must refuse: {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn an_archive_reason_fails_closed_on_an_unknown_label() {
        assert_eq!(SourceArchiveReason::ALL.len(), 4);
        for reason in SourceArchiveReason::ALL {
            assert_eq!(SourceArchiveReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(SourceArchiveReason::parse("expired"), None);
        assert_eq!(SourceArchiveReason::parse(""), None);
    }

    #[test]
    fn dismissal_is_an_archive_reason_and_not_a_read_claim() {
        // Ruling J1's distinct bulk dismiss: dismissing declines to ingest.
        // It must never be representable as, or inferred from, reading.
        let dismissed = SourceArchiveDisposition::Archived(SourceArchiveReason::RecipientDismissed);
        assert_ne!(dismissed, SourceArchiveDisposition::Active);
        // There is deliberately no read/handled state on this DTO at all.
        assert_eq!(
            SourceArchiveReason::RecipientDismissed.as_str(),
            "recipient-dismissed"
        );
    }
}
