//! Typed seat-mailbox vocabulary: who a message is addressed to, what it says,
//! and where a reader has got to.
//!
//! # Why a seat recipient is its own identity
//!
//! A [`SeatRecipientId`] is a durable ADDRESS. It is deliberately not an
//! `AgentId`, a `SessionId`, an `AuthorityPrincipal` or any runtime lifecycle
//! identity, and it is not a projection of one either:
//!
//! * an `AgentId` names a governed actor, and two seats may run the same actor;
//! * a `SessionId` names one conversation, which ends;
//! * an `AuthorityPrincipal` answers "who may decide", not "who is this for" —
//!   mailing something to a principal would conflate delivery with authority;
//! * a runtime lifecycle dies with its process, and the whole point of a
//!   durable mailbox is that a recipient SURVIVES process replacement.
//!
//! Any mapping from an external agent/seat naming scheme onto this address is
//! the future transport adapter's explicit job, not an identity law here. There
//! is no supported StateEngine adapter in this workspace, and this module
//! fabricates none.
//!
//! # What this module does not own
//!
//! No subscription, no drain, no notification, no wake, no safe-boundary
//! promotion: those are the runtime's (R3b/R3c) and are blocked on their own
//! admissions. This is vocabulary plus its bounds, and the store owns durability.

use crate::id::opaque_id;
use crate::protocol::ProtocolVersion;
use std::fmt;

/// The largest durable seat address admitted, counted in UTF-8 bytes.
pub const MAX_SEAT_RECIPIENT_ID_BYTES: usize = 256;

/// The largest mailbox message identity admitted, counted in UTF-8 bytes.
pub const MAX_MAILBOX_MESSAGE_ID_BYTES: usize = 256;

/// Why a mailbox identity could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxIdentityError {
    /// Identity text is absent.
    Empty,
    /// Identity text exceeds its owning type's byte bound.
    TooLarge { bytes: usize, max: usize },
}

impl fmt::Display for MailboxIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MailboxIdentityError::Empty => f.write_str("mailbox identity must not be empty"),
            MailboxIdentityError::TooLarge { bytes, max } => write!(
                f,
                "mailbox identity is {bytes} UTF-8 bytes, above the {max}-byte bound; \
                 it is refused, never truncated"
            ),
        }
    }
}

impl std::error::Error for MailboxIdentityError {}

macro_rules! bounded_mailbox_identity {
    ($(#[$meta:meta])* $name:ident, $max:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Constructs the identity or returns the exact bounded refusal.
            pub fn try_new(raw: impl Into<String>) -> Result<Self, MailboxIdentityError> {
                let raw = raw.into();
                if raw.is_empty() {
                    return Err(MailboxIdentityError::Empty);
                }
                let bytes = raw.len();
                if bytes > $max {
                    return Err(MailboxIdentityError::TooLarge { bytes, max: $max });
                }
                Ok(Self(raw))
            }

            /// Compatibility constructor. Validation remains owned by
            /// [`Self::try_new`], so this route cannot bypass the byte bound.
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

bounded_mailbox_identity! {
    /// A durable seat mailbox address.
    ///
    /// Opaque, non-empty and bounded; the same address survives a process
    /// replacement, which is what makes durable mail deliverable at all.
    /// Distinct nominal type, so the compiler refuses a `SessionId`, `AgentId`
    /// or any other identity where a recipient is required
    /// (`tests/probes/50_seat_recipient_is_not_a_session.rs`).
    SeatRecipientId, MAX_SEAT_RECIPIENT_ID_BYTES
}

bounded_mailbox_identity! {
    /// One mailbox message's own identity, minted by the sender's transport.
    ///
    /// Distinct from [`SeatRecipientId`]: an address is not a message, and the
    /// exact-retry law compares message identity against message identity.
    MailboxMessageId, MAX_MAILBOX_MESSAGE_ID_BYTES
}

opaque_id! {
    /// The SOURCE's position for one message, retained verbatim for replay.
    ///
    /// This is transport truth, not ours: it is stored so a reconnecting
    /// transport can resume, and it is never interpreted, ordered or compared
    /// against our own local sequence. Our ordering authority is
    /// [`MailboxAdmissionSeq`], which is local and monotonic per recipient.
    MailboxSourceCursor
}

/// The largest authoritative body this vocabulary admits, in UTF-8 bytes.
///
/// Pinned as its own literal. A body above the bound is REFUSED, never
/// truncated: the authoritative body is the message, and a silently shortened
/// instruction is worse than a rejected one.
pub const MAX_MAILBOX_BODY_BYTES: usize = 65_536;

/// The authoritative text of one mailbox message.
///
/// Bounded at construction, so an oversize body cannot exist as a value — the
/// refusal happens before any store, page or projection can inherit it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MailboxBody(String);

impl MailboxBody {
    /// Validates the bound. An empty body is refused too: a message with
    /// nothing authoritative in it cannot become work.
    pub fn new(raw: impl Into<String>) -> Result<Self, MailboxError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Err(MailboxError::EmptyBody);
        }
        let bytes = raw.len();
        if bytes > MAX_MAILBOX_BODY_BYTES {
            return Err(MailboxError::BodyTooLarge {
                bytes,
                max: MAX_MAILBOX_BODY_BYTES,
            });
        }
        Ok(MailboxBody(raw))
    }

    /// A body that arrived EMPTY from a source floor; reachable only through
    /// [`Self::received`], so a send path cannot build one by name.
    fn received_empty() -> Self {
        MailboxBody(String::new())
    }

    /// A body as a source floor delivered it: like [`Self::new`], except an
    /// empty body is admitted as [`Self::received_empty`] rather than refused.
    pub fn received(raw: impl Into<String>) -> Result<Self, MailboxError> {
        let raw = raw.into();
        if raw.is_empty() {
            return Ok(Self::received_empty());
        }
        Self::new(raw)
    }

    /// True only for an empty body admitted through [`Self::received`].
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Exact UTF-8 length, which is what both bounds are counted in.
    pub fn len_bytes(&self) -> usize {
        self.0.len()
    }
}

/// A recipient-local, monotonic admission position.
///
/// This is OUR ordering authority, assigned when a message becomes durable
/// here. It is not the source's cursor and carries no transport meaning: two
/// recipients' sequences are independent and neither is comparable with
/// [`MailboxSourceCursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MailboxAdmissionSeq(u64);

impl MailboxAdmissionSeq {
    /// The first position a recipient's mailbox uses.
    ///
    /// One, not zero, so that [`MailboxCursor::START`] (zero, "before
    /// everything") can never collide with a real row's position.
    pub const FIRST: MailboxAdmissionSeq = MailboxAdmissionSeq(1);

    /// Zero is refused: it is the cursor's "before everything" value, and a row
    /// sitting there would be unreachable by an exclusive read.
    pub fn new(value: u64) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(MailboxAdmissionSeq(value))
        }
    }

    pub fn value(self) -> u64 {
        self.0
    }

    /// The next position, or `None` at exhaustion — never a wrap.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(MailboxAdmissionSeq)
    }
}

/// A reader's EXCLUSIVE position in one recipient's mailbox.
///
/// Deliberately a different type from [`MailboxAdmissionSeq`]: "where a row is"
/// and "what a reader has already seen" are different facts, and passing one
/// for the other is the classic off-by-one that silently drops or repeats a
/// message. A page returns rows strictly AFTER this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MailboxCursor(u64);

impl MailboxCursor {
    /// Before every possible row.
    pub const START: MailboxCursor = MailboxCursor(0);

    /// The cursor a reader holds once it has consumed through `seq`.
    pub fn after(seq: MailboxAdmissionSeq) -> Self {
        MailboxCursor(seq.value())
    }

    pub fn value(self) -> u64 {
        self.0
    }
}

/// The largest kind label carried verbatim for a kind this build does not know.
pub const MAX_MAIL_KIND_LABEL_BYTES: usize = 128;

/// The exact label of a mail kind this build does not know.
///
/// Source floors add kinds over time. A row of a new kind is still mail: it is
/// listed, readable and acknowledgeable, with its kind shown verbatim. Bounded,
/// non-empty and free of control characters; a known label never becomes one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MailKindLabel(String);

impl MailKindLabel {
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        let admissible = !raw.is_empty()
            && raw.len() <= MAX_MAIL_KIND_LABEL_BYTES
            && !raw.chars().any(char::is_control)
            && SeatMessageKind::KNOWN
                .iter()
                .all(|kind| kind.as_str() != raw);
        admissible.then_some(MailKindLabel(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The kinds a seat mailbox may carry.
///
/// The known kinds are exhaustive for owners that act on them. Any other label
/// is [`Self::Other`]: ordinary mail that is shown and read, never routed as a
/// known kind and never dropped.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SeatMessageKind {
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
    ///
    /// Admitted by the R3b-2A type amendment under ruling J5. The original six
    /// kinds had no home for a settled ruling, so a `ruling` arriving from a
    /// remote floor could only be dropped or mapped onto [`Self::RulingRequest`]
    /// — turning a decision back into an open question. Both are refused; the
    /// kind is its own.
    Ruling,
    /// A kind this build does not know, kept verbatim.
    Other(MailKindLabel),
}

impl SeatMessageKind {
    /// Every known kind. The decoder consults this list first, so a known
    /// variant missing from it would decode as [`Self::Other`].
    pub const KNOWN: [SeatMessageKind; 8] = [
        SeatMessageKind::TaskPointer,
        SeatMessageKind::Question,
        SeatMessageKind::Blocker,
        SeatMessageKind::Done,
        SeatMessageKind::Handoff,
        SeatMessageKind::Finding,
        SeatMessageKind::RulingRequest,
        SeatMessageKind::Ruling,
    ];

    pub fn as_str(&self) -> &str {
        match self {
            SeatMessageKind::TaskPointer => "task-pointer",
            SeatMessageKind::Question => "question",
            SeatMessageKind::Blocker => "blocker",
            SeatMessageKind::Done => "done",
            SeatMessageKind::Handoff => "handoff",
            SeatMessageKind::Finding => "finding",
            SeatMessageKind::RulingRequest => "ruling-request",
            SeatMessageKind::Ruling => "ruling",
            SeatMessageKind::Other(label) => label.as_str(),
        }
    }

    /// A known label decodes to its own kind, never a neighbour; any other
    /// admissible label is [`Self::Other`]. `None` only for a label that
    /// [`MailKindLabel`] refuses.
    pub fn parse(raw: &str) -> Option<Self> {
        SeatMessageKind::KNOWN
            .into_iter()
            .find(|kind| kind.as_str() == raw)
            .or_else(|| MailKindLabel::new(raw).map(SeatMessageKind::Other))
    }
}

/// Why an offered mailbox message is not admissible as a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxError {
    /// A message with no authoritative body.
    EmptyBody,
    /// Above [`MAX_MAILBOX_BODY_BYTES`]. Carried, never truncated.
    BodyTooLarge { bytes: usize, max: usize },
    /// A message that supersedes itself: no edge can be its own predecessor.
    SupersedesSelf { message: String },
    /// A message addressed to the sender's own address.
    SenderIsRecipient { address: String },
}

impl fmt::Display for MailboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MailboxError::EmptyBody => f.write_str("a mailbox body must not be empty"),
            MailboxError::BodyTooLarge { bytes, max } => write!(
                f,
                "mailbox body is {bytes} UTF-8 bytes, above the {max}-byte bound; \
                 it is refused, never truncated"
            ),
            MailboxError::SupersedesSelf { message } => {
                write!(f, "message {message:?} cannot supersede itself")
            }
            MailboxError::SenderIsRecipient { address } => {
                write!(f, "a seat cannot mail itself at {address:?}")
            }
        }
    }
}

impl std::error::Error for MailboxError {}

/// One typed seat mailbox message.
///
/// Every field is validated at construction and the struct's fields are
/// private, so a decoder cannot assemble one out of a raw transport map: the
/// only route in is [`SeatMailboxMessage::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatMailboxMessage {
    protocol: ProtocolVersion,
    id: MailboxMessageId,
    sender: SeatRecipientId,
    recipient: SeatRecipientId,
    kind: SeatMessageKind,
    body: MailboxBody,
    supersedes: Option<MailboxMessageId>,
    source: MailboxSourceCursor,
}

impl SeatMailboxMessage {
    /// The one constructor.
    ///
    /// Refuses self-supersession and self-addressing here, where the values are
    /// present. Every other supersession law — the target must EXIST, belong to
    /// the SAME recipient and be strictly EARLIER — needs durable neighbours and
    /// therefore belongs to the store, not to a value.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        protocol: ProtocolVersion,
        id: MailboxMessageId,
        sender: SeatRecipientId,
        recipient: SeatRecipientId,
        kind: SeatMessageKind,
        body: MailboxBody,
        supersedes: Option<MailboxMessageId>,
        source: MailboxSourceCursor,
    ) -> Result<Self, MailboxError> {
        if supersedes.as_ref() == Some(&id) {
            return Err(MailboxError::SupersedesSelf {
                message: id.as_str().to_owned(),
            });
        }
        if sender == recipient {
            return Err(MailboxError::SenderIsRecipient {
                address: recipient.as_str().to_owned(),
            });
        }
        Ok(SeatMailboxMessage {
            protocol,
            id,
            sender,
            recipient,
            kind,
            body,
            supersedes,
            source,
        })
    }

    pub fn protocol(&self) -> &ProtocolVersion {
        &self.protocol
    }

    pub fn id(&self) -> &MailboxMessageId {
        &self.id
    }

    pub fn sender(&self) -> &SeatRecipientId {
        &self.sender
    }

    pub fn recipient(&self) -> &SeatRecipientId {
        &self.recipient
    }

    pub fn kind(&self) -> &SeatMessageKind {
        &self.kind
    }

    pub fn body(&self) -> &MailboxBody {
        &self.body
    }

    pub fn supersedes(&self) -> Option<&MailboxMessageId> {
        self.supersedes.as_ref()
    }

    pub fn source(&self) -> &MailboxSourceCursor {
        &self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn message(id: &str) -> Result<SeatMailboxMessage, Box<dyn std::error::Error>> {
        Ok(SeatMailboxMessage::new(
            ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
            MailboxMessageId::new(id).ok_or("id")?,
            SeatRecipientId::new("seat-alpha").ok_or("sender")?,
            SeatRecipientId::new("seat-gamma").ok_or("recipient")?,
            SeatMessageKind::TaskPointer,
            MailboxBody::new("do the thing")?,
            None,
            MailboxSourceCursor::new("src-1").ok_or("source")?,
        )?)
    }

    #[test]
    fn a_body_at_the_bound_is_admitted_and_one_byte_over_is_refused() -> TestResult {
        let at_bound = "a".repeat(MAX_MAILBOX_BODY_BYTES);
        let body = MailboxBody::new(at_bound)?;
        assert_eq!(body.len_bytes(), MAX_MAILBOX_BODY_BYTES);

        let over = "a".repeat(MAX_MAILBOX_BODY_BYTES + 1);
        match MailboxBody::new(over) {
            Err(MailboxError::BodyTooLarge { bytes, max }) => {
                assert_eq!(bytes, MAX_MAILBOX_BODY_BYTES + 1);
                assert_eq!(max, MAX_MAILBOX_BODY_BYTES);
            }
            other => panic!("an oversize body must refuse, never truncate: {other:?}"),
        }
        assert!(matches!(MailboxBody::new(""), Err(MailboxError::EmptyBody)));
        Ok(())
    }

    #[test]
    fn the_body_bound_is_counted_in_utf8_bytes_not_characters() -> TestResult {
        // A multi-byte character makes "length in chars" and "length in bytes"
        // disagree; the bound the store and the page both count is BYTES.
        let wide = "\u{1F600}".repeat(MAX_MAILBOX_BODY_BYTES / 4);
        let body = MailboxBody::new(wide)?;
        assert_eq!(body.len_bytes(), MAX_MAILBOX_BODY_BYTES);
        assert!(body.as_str().chars().count() < MAX_MAILBOX_BODY_BYTES);
        Ok(())
    }

    #[test]
    fn a_cursor_is_exclusive_and_starts_before_the_first_position() {
        // START must sit strictly below FIRST or the very first message would
        // be skipped by its own reader.
        assert_eq!(MailboxCursor::START.value(), 0);
        assert_eq!(MailboxAdmissionSeq::FIRST.value(), 1);
        assert!(MailboxCursor::START.value() < MailboxAdmissionSeq::FIRST.value());
        assert!(MailboxAdmissionSeq::new(0).is_none());

        let seq = MailboxAdmissionSeq::FIRST;
        assert_eq!(MailboxCursor::after(seq).value(), seq.value());
    }

    #[test]
    fn the_admission_sequence_refuses_to_wrap() -> TestResult {
        let last = MailboxAdmissionSeq::new(u64::MAX).ok_or("max is a valid position")?;
        assert!(
            last.next().is_none(),
            "exhaustion must refuse, never wrap back onto a used position"
        );
        Ok(())
    }

    #[test]
    fn the_known_kinds_round_trip_and_other_labels_stay_verbatim() {
        assert_eq!(SeatMessageKind::KNOWN.len(), 8);
        for kind in SeatMessageKind::KNOWN {
            assert_eq!(SeatMessageKind::parse(kind.as_str()), Some(kind));
        }
        // A label this build does not know is still mail, kept verbatim.
        let other = SeatMessageKind::parse("ping");
        assert_eq!(other.as_ref().map(SeatMessageKind::as_str), Some("ping"));
        assert!(matches!(other, Some(SeatMessageKind::Other(_))));
        assert_eq!(SeatMessageKind::parse(""), None);
        // Distinct labels, or two kinds would decode to one.
        let labels: std::collections::BTreeSet<&str> =
            SeatMessageKind::KNOWN.iter().map(|k| k.as_str()).collect();
        assert_eq!(labels.len(), SeatMessageKind::KNOWN.len());
    }

    #[test]
    fn a_message_cannot_supersede_itself_or_address_its_own_sender() -> TestResult {
        let ok = message("msg-1")?;
        assert_eq!(ok.id().as_str(), "msg-1");
        assert!(ok.supersedes().is_none());

        let self_edge = SeatMailboxMessage::new(
            ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
            MailboxMessageId::new("msg-1").ok_or("id")?,
            SeatRecipientId::new("seat-alpha").ok_or("sender")?,
            SeatRecipientId::new("seat-gamma").ok_or("recipient")?,
            SeatMessageKind::Done,
            MailboxBody::new("done")?,
            Some(MailboxMessageId::new("msg-1").ok_or("target")?),
            MailboxSourceCursor::new("src-1").ok_or("source")?,
        );
        assert!(matches!(
            self_edge,
            Err(MailboxError::SupersedesSelf { .. })
        ));

        let self_mail = SeatMailboxMessage::new(
            ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
            MailboxMessageId::new("msg-2").ok_or("id")?,
            SeatRecipientId::new("seat-gamma").ok_or("sender")?,
            SeatRecipientId::new("seat-gamma").ok_or("recipient")?,
            SeatMessageKind::Done,
            MailboxBody::new("done")?,
            None,
            MailboxSourceCursor::new("src-2").ok_or("source")?,
        );
        assert!(matches!(
            self_mail,
            Err(MailboxError::SenderIsRecipient { .. })
        ));
        Ok(())
    }

    #[test]
    fn identities_are_non_empty_and_nominally_distinct() -> TestResult {
        assert_eq!(
            SeatRecipientId::try_new(""),
            Err(MailboxIdentityError::Empty)
        );
        assert_eq!(
            MailboxMessageId::try_new(""),
            Err(MailboxIdentityError::Empty)
        );
        assert!(SeatRecipientId::new("").is_none());
        assert!(MailboxMessageId::new("").is_none());
        assert!(MailboxSourceCursor::new("").is_none());
        // Same text, different types: equality is not even expressible across
        // them, which is the compile-fail probe's runtime shadow.
        let recipient = SeatRecipientId::new("same").ok_or("recipient")?;
        let message = MailboxMessageId::new("same").ok_or("message")?;
        assert_eq!(recipient.as_str(), message.as_str());
        Ok(())
    }

    #[test]
    fn a_seat_recipient_at_its_bound_is_exact_and_one_byte_over_refuses() -> TestResult {
        assert_eq!(MAX_SEAT_RECIPIENT_ID_BYTES, 256);
        let at_bound = "r".repeat(256);
        let recipient = SeatRecipientId::try_new(at_bound.clone())?;
        assert_eq!(recipient.as_str(), at_bound);

        let over = "r".repeat(257);
        assert_eq!(
            SeatRecipientId::try_new(over),
            Err(MailboxIdentityError::TooLarge {
                bytes: 257,
                max: 256,
            })
        );
        Ok(())
    }

    #[test]
    fn a_message_id_at_its_bound_is_exact_and_one_byte_over_refuses() -> TestResult {
        assert_eq!(MAX_MAILBOX_MESSAGE_ID_BYTES, 256);
        let at_bound = "m".repeat(256);
        let id = MailboxMessageId::try_new(at_bound.clone())?;
        assert_eq!(id.as_str(), at_bound);

        let over = "m".repeat(257);
        assert_eq!(
            MailboxMessageId::try_new(over),
            Err(MailboxIdentityError::TooLarge {
                bytes: 257,
                max: 256,
            })
        );
        Ok(())
    }

    #[test]
    fn mailbox_identity_bounds_count_utf8_bytes_not_characters() -> TestResult {
        let at_bound = "\u{1F600}".repeat(MAX_SEAT_RECIPIENT_ID_BYTES / 4);
        let recipient = SeatRecipientId::try_new(at_bound)?;
        assert_eq!(recipient.as_str().len(), MAX_SEAT_RECIPIENT_ID_BYTES);

        let over = "\u{1F600}".repeat((MAX_MAILBOX_MESSAGE_ID_BYTES / 4) + 1);
        match MailboxMessageId::try_new(over) {
            Err(MailboxIdentityError::TooLarge { bytes, max }) => {
                assert_eq!(bytes, MAX_MAILBOX_MESSAGE_ID_BYTES + 4);
                assert_eq!(max, MAX_MAILBOX_MESSAGE_ID_BYTES);
            }
            other => panic!("a multi-byte over-bound identity must refuse by bytes: {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn an_over_bound_identity_is_never_truncated_into_a_value() {
        let over = "z".repeat(MAX_SEAT_RECIPIENT_ID_BYTES + 1);
        match SeatRecipientId::try_new(over) {
            Err(MailboxIdentityError::TooLarge { bytes, max }) => {
                assert_eq!(bytes, MAX_SEAT_RECIPIENT_ID_BYTES + 1);
                assert_eq!(max, MAX_SEAT_RECIPIENT_ID_BYTES);
            }
            other => panic!("an over-bound identity must not become a shortened value: {other:?}"),
        }
    }

    #[test]
    fn compatibility_constructors_cannot_bypass_the_typed_bound() {
        assert!(SeatRecipientId::new("r".repeat(MAX_SEAT_RECIPIENT_ID_BYTES + 1)).is_none());
        assert!(MailboxMessageId::new("m".repeat(MAX_MAILBOX_MESSAGE_ID_BYTES + 1)).is_none());
    }
}
