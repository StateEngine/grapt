//! Snapshot, barrier, cursor and invalidation.
//!
//! # The race this closes (ruling J3)
//!
//! There is exactly one admitted handshake, `subscribe_and_snapshot`. Its single
//! linearization point both arms a subscription covering every mutation strictly
//! AFTER a returned [`SnapshotBarrier`], and fixes a stable snapshot containing
//! everything committed at or before it. There is no list-then-subscribe and no
//! subscribe-then-unbounded-list path, and J3 explicitly refuses a v1 multi-call
//! fallback: observing that a page came back empty proves nothing about what was
//! committed while the observer was not listening.
//!
//! # Invalidation is never emptiness (ruling J4)
//!
//! `CursorInvalid` is not end-of-stream. It carries a reason and a recovery, and
//! the recovery is either a no-skip [`SafeReplayOrigin`] guaranteed to cover the
//! checkpoint, or an explicit refusal. Jumping to the head, inferring empty, or
//! ageing a cursor out locally are all forbidden — losing mail silently is worse
//! than pausing the path loudly.

use crate::identity::{
    ExclusiveSourceCursor, MailSnapshotId, MailSubscriptionId, SafeReplayOrigin, SnapshotBarrier,
};
use crate::message::BoundRemoteSeatMailMessageV1;
use core::fmt;

/// Why a cursor stopped being usable.
///
/// # This set is a proposal, not provider evidence
///
/// The reconciliation addendum's amendment 3 is explicit: the exact wire tags
/// belong to the ratified provider contract, and these names are a MINIMUM
/// SEMANTIC CATEGORY set. They are modelled so the laws below can be stated and
/// tested now; the adapter that eventually meets a real provider maps its
/// ratified tags onto these, and a tag with no category here is a decode
/// failure rather than a silently accepted unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorInvalidReason {
    /// The issuer aged the cursor out.
    Expired,
    /// The source compacted history through the cursor's position.
    Compacted,
    /// The cursor belongs to a different binding.
    WrongBinding,
    /// The cursor belongs to a different recipient.
    WrongRecipient,
    /// The cursor belongs to a different protocol version.
    WrongProtocol,
    /// The cursor names a position earlier than one already settled.
    Regressed,
    /// The cursor did not decode.
    Malformed,
    /// The issuer restarted and cannot offer a compatible replay.
    IssuerRestartedWithoutCompatibleReplay,
}

impl CursorInvalidReason {
    pub const ALL: [CursorInvalidReason; 8] = [
        CursorInvalidReason::Expired,
        CursorInvalidReason::Compacted,
        CursorInvalidReason::WrongBinding,
        CursorInvalidReason::WrongRecipient,
        CursorInvalidReason::WrongProtocol,
        CursorInvalidReason::Regressed,
        CursorInvalidReason::Malformed,
        CursorInvalidReason::IssuerRestartedWithoutCompatibleReplay,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            CursorInvalidReason::Expired => "expired",
            CursorInvalidReason::Compacted => "compacted",
            CursorInvalidReason::WrongBinding => "wrong-binding",
            CursorInvalidReason::WrongRecipient => "wrong-recipient",
            CursorInvalidReason::WrongProtocol => "wrong-protocol",
            CursorInvalidReason::Regressed => "regressed",
            CursorInvalidReason::Malformed => "malformed",
            CursorInvalidReason::IssuerRestartedWithoutCompatibleReplay => {
                "issuer-restarted-without-compatible-replay"
            }
        }
    }

    /// Fails closed. There is deliberately no `Other(String)`: a reason that
    /// arrived as free text could not be reasoned about by a recovery policy.
    pub fn parse(raw: &str) -> Option<Self> {
        CursorInvalidReason::ALL
            .into_iter()
            .find(|reason| reason.as_str() == raw)
    }
}

/// What a reader may do after invalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorRecovery {
    /// Enumerate every retained message from this origin.
    ///
    /// The provider guarantees the origin COVERS the reader's checkpoint, so no
    /// message between the two can be skipped. Exact immutable source identity
    /// then makes re-admission and re-acknowledgement idempotent.
    ReplayFrom(SafeReplayOrigin),
    /// No origin exists that can cover the checkpoint without skipping.
    ///
    /// The mailbox path blocks explicitly. This is the honest end of J4: when
    /// the provider cannot prove no-skip recovery, pausing is correct and
    /// resuming from the head would be silent loss.
    RecoveryUnavailable { detail: &'static str },
}

/// A cursor invalidation: a reason plus its recovery, never one without the
/// other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorInvalidation {
    reason: CursorInvalidReason,
    recovery: CursorRecovery,
}

impl CursorInvalidation {
    pub fn new(reason: CursorInvalidReason, recovery: CursorRecovery) -> Self {
        CursorInvalidation { reason, recovery }
    }

    pub fn reason(&self) -> CursorInvalidReason {
        self.reason
    }

    pub fn recovery(&self) -> &CursorRecovery {
        &self.recovery
    }

    /// True when the reader may continue without skipping anything.
    pub fn is_replayable(&self) -> bool {
        matches!(self.recovery, CursorRecovery::ReplayFrom(_))
    }
}

impl fmt::Display for CursorInvalidation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.recovery {
            CursorRecovery::ReplayFrom(origin) => write!(
                f,
                "cursor invalid ({}); replay from {origin} — this is not end of stream",
                self.reason.as_str()
            ),
            CursorRecovery::RecoveryUnavailable { detail } => write!(
                f,
                "cursor invalid ({}) and no no-skip replay origin exists ({detail}); \
                 the mailbox path blocks rather than resuming from the head",
                self.reason.as_str()
            ),
        }
    }
}

/// One armed subscription's generation.
///
/// Monotonic per binding, and deliberately NOT a fence. Ruling J2 removed
/// fencing, so this value orders and identifies generations for diagnosis and
/// for discarding stale in-flight signals within one process. It never rejects
/// another owner's write; duplicate admission is prevented downstream by exact
/// immutable source identity, not by this number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubscriptionGeneration(u64);

impl SubscriptionGeneration {
    pub const FIRST: SubscriptionGeneration = SubscriptionGeneration(1);

    /// Zero is refused so "no generation yet" cannot be confused with one.
    pub fn new(value: u64) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(SubscriptionGeneration(value))
        }
    }

    pub fn value(self) -> u64 {
        self.0
    }

    /// The next generation, or `None` at exhaustion — never a wrap.
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(SubscriptionGeneration)
    }
}

/// A stable snapshot fixed at the barrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailSnapshot {
    snapshot_id: MailSnapshotId,
    barrier: SnapshotBarrier,
    start_after: Option<ExclusiveSourceCursor>,
    retained_origin: SafeReplayOrigin,
}

impl MailSnapshot {
    pub fn new(
        snapshot_id: MailSnapshotId,
        barrier: SnapshotBarrier,
        start_after: Option<ExclusiveSourceCursor>,
        retained_origin: SafeReplayOrigin,
    ) -> Self {
        MailSnapshot {
            snapshot_id,
            barrier,
            start_after,
            retained_origin,
        }
    }

    pub fn snapshot_id(&self) -> &MailSnapshotId {
        &self.snapshot_id
    }

    pub fn barrier(&self) -> &SnapshotBarrier {
        &self.barrier
    }

    /// The exclusive resume point, or `None` to start at the retained origin.
    ///
    /// `None` means "from the origin", never "from the head": an absent cursor
    /// on a first connect must not skip the mail that is already waiting.
    pub fn start_after(&self) -> Option<&ExclusiveSourceCursor> {
        self.start_after.as_ref()
    }

    pub fn retained_origin(&self) -> &SafeReplayOrigin {
        &self.retained_origin
    }
}

/// The single atomic result of the one admitted handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedMailboxSubscription {
    subscription_id: MailSubscriptionId,
    generation: SubscriptionGeneration,
    snapshot: MailSnapshot,
}

impl ArmedMailboxSubscription {
    pub fn new(
        subscription_id: MailSubscriptionId,
        generation: SubscriptionGeneration,
        snapshot: MailSnapshot,
    ) -> Self {
        ArmedMailboxSubscription {
            subscription_id,
            generation,
            snapshot,
        }
    }

    pub fn subscription_id(&self) -> &MailSubscriptionId {
        &self.subscription_id
    }

    pub fn generation(&self) -> SubscriptionGeneration {
        self.generation
    }

    pub fn snapshot(&self) -> &MailSnapshot {
        &self.snapshot
    }
}

/// One bounded page of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteMailPage {
    messages: Vec<BoundRemoteSeatMailMessageV1>,
    next_after: Option<ExclusiveSourceCursor>,
    v2: Option<RefusalPageProgress>,
    decode_failures: Vec<crate::MailDecodeFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusalPageProgress {
    refused: Vec<crate::RefusedMailRow>,
    last: Option<ExclusiveSourceCursor>,
}

impl RemoteMailPage {
    /// A page that ended at a count or byte boundary.
    ///
    /// `next_after` names the LAST FULLY EMITTED message. A partial message is
    /// never emitted, so a resume from this cursor can neither repeat a whole
    /// message nor start mid-one.
    pub fn bounded(
        messages: Vec<BoundRemoteSeatMailMessageV1>,
        next_after: ExclusiveSourceCursor,
    ) -> Self {
        RemoteMailPage {
            messages,
            next_after: Some(next_after),
            v2: None,
            decode_failures: Vec::new(),
        }
    }

    /// A page that exhausted the snapshot.
    ///
    /// `next_after` is absent precisely because nothing remains; that is a
    /// different fact from a bounded page and the two must not be conflated.
    pub fn final_page(messages: Vec<BoundRemoteSeatMailMessageV1>) -> Self {
        RemoteMailPage {
            messages,
            next_after: None,
            v2: None,
            decode_failures: Vec::new(),
        }
    }

    /// The supplied combined last entry is opaque. It must name an emitted
    /// entry; no cursor parsing or cross-array lexical ordering is used.
    pub fn v2_page(
        messages: Vec<BoundRemoteSeatMailMessageV1>,
        refused: Vec<crate::RefusedMailRow>,
        next_after: Option<ExclusiveSourceCursor>,
        last: Option<ExclusiveSourceCursor>,
    ) -> Option<Self> {
        Self::v2_page_with_decode_failures(messages, refused, Vec::new(), next_after, last)
    }

    /// Local decoder failures remain separate from source refusal coordinates.
    pub fn v2_page_with_decode_failures(
        messages: Vec<BoundRemoteSeatMailMessageV1>,
        refused: Vec<crate::RefusedMailRow>,
        decode_failures: Vec<crate::MailDecodeFailure>,
        next_after: Option<ExclusiveSourceCursor>,
        last: Option<ExclusiveSourceCursor>,
    ) -> Option<Self> {
        let count = messages
            .len()
            .checked_add(refused.len())?
            .checked_add(decode_failures.len())?;
        if count > 64
            || (count == 0) != last.is_none()
            || (next_after.is_some() && next_after != last)
        {
            return None;
        }
        let mut cursors = std::collections::BTreeSet::new();
        for cursor in messages
            .iter()
            .map(|m| m.source_cursor())
            .chain(refused.iter().map(|r| r.cursor()))
            .chain(decode_failures.iter().map(|r| r.cursor()))
        {
            if !cursors.insert(cursor) {
                return None;
            }
        }
        if last.as_ref().is_some_and(|cursor| {
            messages.last().map(|m| m.source_cursor()) != Some(cursor)
                && refused.last().map(|r| r.cursor()) != Some(cursor)
                && decode_failures.last().map(|r| r.cursor()) != Some(cursor)
        }) {
            return None;
        }
        if refused
            .windows(2)
            .any(|rows| rows[0].end() > rows[1].start())
        {
            return None;
        }
        Some(Self {
            messages,
            next_after,
            v2: Some(RefusalPageProgress { refused, last }),
            decode_failures,
        })
    }
    /// V1 can skip unknown kinds in memory; a restart re-enumerates the source.
    pub fn v1_page_with_decode_failures(
        messages: Vec<BoundRemoteSeatMailMessageV1>,
        decode_failures: Vec<crate::MailDecodeFailure>,
        next_after: Option<ExclusiveSourceCursor>,
    ) -> Option<Self> {
        let last = next_after
            .clone()
            .or_else(|| messages.last().map(|m| m.source_cursor().clone()))
            .or_else(|| decode_failures.last().map(|f| f.cursor().clone()));
        let mut page = Self::v2_page_with_decode_failures(
            messages,
            Vec::new(),
            decode_failures,
            next_after,
            last,
        )?;
        page.v2 = None;
        Some(page)
    }
    pub fn decode_failures(&self) -> &[crate::MailDecodeFailure] {
        &self.decode_failures
    }
    pub fn refused_rows(&self) -> &[crate::RefusedMailRow] {
        self.v2
            .as_ref()
            .map_or(&[], |progress| progress.refused.as_slice())
    }
    pub fn last_cursor(&self) -> Option<&ExclusiveSourceCursor> {
        self.v2.as_ref().and_then(|progress| progress.last.as_ref())
    }
    pub fn is_v2(&self) -> bool {
        self.v2.is_some()
    }
    pub fn entry_count(&self) -> usize {
        self.messages.len() + self.refused_rows().len() + self.decode_failures.len()
    }

    pub fn messages(&self) -> &[BoundRemoteSeatMailMessageV1] {
        &self.messages
    }

    pub fn next_after(&self) -> Option<&ExclusiveSourceCursor> {
        self.next_after.as_ref()
    }

    /// True when the snapshot has more to give.
    pub fn has_more(&self) -> bool {
        self.next_after.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Result<SafeReplayOrigin, Box<dyn std::error::Error>> {
        Ok(SafeReplayOrigin::new("origin-1").ok_or("origin")?)
    }

    #[test]
    fn every_invalidation_reason_has_a_distinct_label_and_fails_closed() {
        assert_eq!(CursorInvalidReason::ALL.len(), 8);
        for reason in CursorInvalidReason::ALL {
            assert_eq!(CursorInvalidReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(CursorInvalidReason::parse("other"), None);
        assert_eq!(CursorInvalidReason::parse(""), None);
        let labels: std::collections::BTreeSet<&str> = CursorInvalidReason::ALL
            .iter()
            .map(|r| r.as_str())
            .collect();
        assert_eq!(labels.len(), CursorInvalidReason::ALL.len());
    }

    #[test]
    fn invalidation_is_never_end_of_stream_and_says_so() -> Result<(), Box<dyn std::error::Error>> {
        let replayable = CursorInvalidation::new(
            CursorInvalidReason::Expired,
            CursorRecovery::ReplayFrom(origin()?),
        );
        assert!(replayable.is_replayable());
        assert!(replayable.to_string().contains("not end of stream"));

        let blocked = CursorInvalidation::new(
            CursorInvalidReason::Compacted,
            CursorRecovery::RecoveryUnavailable {
                detail: "history behind the checkpoint was compacted away",
            },
        );
        assert!(!blocked.is_replayable());
        // The refusal must be visible as a refusal, not as an empty result.
        assert!(blocked.to_string().contains("blocks"));
        Ok(())
    }

    #[test]
    fn a_snapshot_without_a_resume_cursor_starts_at_the_origin_not_the_head(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = MailSnapshot::new(
            MailSnapshotId::new("snap-1").ok_or("snapshot")?,
            SnapshotBarrier::new("barrier-1").ok_or("barrier")?,
            None,
            origin()?,
        );
        // The named resurrection is "absent cursor means start at the head",
        // which would skip everything already waiting on a first connect.
        assert!(snapshot.start_after().is_none());
        assert_eq!(snapshot.retained_origin().as_str(), "origin-1");
        Ok(())
    }

    #[test]
    fn a_bounded_page_and_an_exhausted_page_are_different_facts(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let bounded = RemoteMailPage::bounded(
            Vec::new(),
            ExclusiveSourceCursor::new("cur-9").ok_or("cursor")?,
        );
        assert!(bounded.has_more());
        assert_eq!(
            bounded.next_after().map(ExclusiveSourceCursor::as_str),
            Some("cur-9")
        );

        let done = RemoteMailPage::final_page(Vec::new());
        assert!(!done.has_more());
        assert!(done.next_after().is_none());
        // Both are empty of messages here; only `has_more` separates them, which
        // is exactly why an empty page may never be read as "nothing remains".
        assert_eq!(bounded.messages().len(), done.messages().len());
        assert_ne!(bounded.has_more(), done.has_more());
        Ok(())
    }

    #[test]
    fn a_subscription_generation_refuses_zero_and_refuses_to_wrap(
    ) -> Result<(), Box<dyn std::error::Error>> {
        assert!(SubscriptionGeneration::new(0).is_none());
        assert_eq!(SubscriptionGeneration::FIRST.value(), 1);
        let last = SubscriptionGeneration::new(u64::MAX).ok_or("max is valid")?;
        assert!(
            last.next().is_none(),
            "exhaustion must refuse, never wrap onto a used generation"
        );
        Ok(())
    }
}
