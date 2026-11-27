//! Source integrity observations and explicit transport context. A refused row
//! has no sender, body or admitted-message identity and grants no mail authority.
use super::{ExclusiveSourceCursor, IdentityError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailTransportVersion {
    V1,
    V2,
}
impl MailTransportVersion {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "remote-seat-mail-transport/v1",
            Self::V2 => "remote-seat-mail-transport/v2",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [Self::V1, Self::V2]
            .into_iter()
            .find(|protocol| protocol.as_str() == value)
    }
    pub const fn adapter(self) -> &'static str {
        match self {
            Self::V1 => "stateengine-native-mail/1",
            Self::V2 => "stateengine-native-mail/2",
        }
    }
}

macro_rules! closed_reasons {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const fn as_str(self) -> &'static str { match self { $(Self::$variant => $wire),+ } }
            pub fn parse(value: &str) -> Option<Self> { match value { $($wire => Some(Self::$variant)),+, _ => None } }
        }
    };
}
// Closed provider vocabulary, including fixed schema codes retained for older
// StateEngine v2 providers. New providers fold schema details into MalformedEnvelope.
closed_reasons! { RefusedRowReason {
    MalformedJson => "malformed_json",
    LineTooLarge => "line_too_large",
    DuplicateMessageId => "duplicate_msg_id",
    AmbiguousDuplicateAck => "ack_ambiguous_duplicate_msg_id",
    RecordObject => "schema:record_object",
    RecordType => "schema:record_type",
    MessageId => "schema:message.msg_id",
    MessageTimestamp => "schema:message.ts",
    MessageSender => "schema:message.from_seat",
    MessageRecipient => "schema:message.to_seat",
    MessageKind => "schema:message.kind",
    MessageReferences => "schema:message.refs",
    MessageReference => "schema:message.ref",
    MessageBody => "schema:message.body",
    MessageWake => "schema:message.work_wake",
    BellOutcome => "schema:bell-outcome",
    SeatProcessExit => "schema:seat-process-exit",
    EngineFollowHalted => "schema:engine-follow-halted",
    SeatIdleAlarm => "schema:seat-idle-alarm",
    PacketUnacknowledgedAlarm => "schema:packet-unacknowledged-alarm",
    NativeDelivery => "schema:native-mail-delivery",
    ReadMarkerId => "schema:read-marker.msg_id",
    ReadMarkerSeat => "schema:read-marker.seat",
    DismissId => "schema:dismiss.msg_id",
    DismissSeat => "schema:dismiss.seat",
    DeliveredId => "schema:delivered.msg_id",
    DeliveredSeat => "schema:delivered.seat",
    DeliveryRetryId => "schema:delivery-retry.msg_id",
    DeliveryRetrySeat => "schema:delivery-retry.seat",
    NativeDeliveryId => "schema:native-mail-delivery.msg_id",
    NativeDeliverySeat => "schema:native-mail-delivery.seat",
    BellHandoffId => "schema:bell-output-handoff.msg_id",
    BellHandoffSeat => "schema:bell-output-handoff.seat",
    MalformedEnvelope => "MalformedEnvelope",
    UnknownKind => "UnknownKind",
    UnknownArchiveReason => "UnknownArchiveReason",
    MessageBodyExceeded => "MessageBodyExceeded",
    PageBodyBytesExceeded => "PageBodyBytesExceeded",
    PageDecodedBytesExceeded => "PageDecodedBytesExceeded",
    ReferenceCountExceeded => "ReferenceCountExceeded",
    ReferenceBytesExceeded => "ReferenceBytesExceeded",
    ProjectProvenanceMissing => "ProjectProvenanceMissing",
    SupersessionConflict => "SupersessionConflict",
} }

/// Untrusted index label. Deliberately not RemoteMailboxMessageId: equality
/// with a real message ID cannot turn an integrity observation into delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedIndexId(String);
impl RefusedIndexId {
    pub fn new(value: String) -> Result<Self, IdentityError> {
        if value.is_empty() {
            return Err(IdentityError::Empty {
                type_name: "RefusedIndexId",
            });
        }
        if value.len() > 256 {
            return Err(IdentityError::TooLarge {
                type_name: "RefusedIndexId",
                bytes: value.len(),
                max: 256,
            });
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedMailRow {
    indexed_id: Option<RefusedIndexId>,
    start: u64,
    end: u64,
    reason: RefusedRowReason,
    cursor: ExclusiveSourceCursor,
}

impl RefusedMailRow {
    pub fn new(
        indexed_id: Option<RefusedIndexId>,
        start: u64,
        end: u64,
        reason: RefusedRowReason,
        cursor: ExclusiveSourceCursor,
    ) -> Option<Self> {
        if end <= start || end > 9_007_199_254_740_991 || cursor.as_str().len() > 144 {
            return None;
        }
        Some(Self {
            indexed_id,
            start,
            end,
            reason,
            cursor,
        })
    }
    pub fn indexed_id(&self) -> Option<&RefusedIndexId> {
        self.indexed_id.as_ref()
    }
    pub fn start(&self) -> u64 {
        self.start
    }
    pub fn end(&self) -> u64 {
        self.end
    }
    pub fn reason(&self) -> RefusedRowReason {
        self.reason
    }
    pub fn cursor(&self) -> &ExclusiveSourceCursor {
        &self.cursor
    }
}

/// Local unknown-kind decode failure. It grants no admission or ACK authority
/// and carries no invented source byte range or untrusted message content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailDecodeFailure {
    cursor: ExclusiveSourceCursor,
}
impl MailDecodeFailure {
    pub fn unknown_kind(cursor: ExclusiveSourceCursor) -> Option<Self> {
        (cursor.as_str().len() <= 144).then_some(Self { cursor })
    }
    pub fn cursor(&self) -> &ExclusiveSourceCursor {
        &self.cursor
    }
}
