//! Advisory live notices, and the type that makes commit-before-notify
//! unforgeable.
//!
//! # The law
//!
//! Ratified §3.2: *a live notice naming a durable fact is published only after
//! that exact fact and its authoritative transactional projections commit
//! successfully*. The supersession matrix states the same rule as "publish only
//! after the exact fact and authoritative projections commit; persistence
//! failure is typed and prevents publication."
//!
//! # How the ordering is carried
//!
//! Not by statement order, and deliberately so. A notice is minted inside
//! [`crate::GovernedTransaction`] while the fact is being written, but the
//! carrier that leaves the transaction — [`CommittedNotices`] — has **no public
//! constructor and exactly one producer**: the `Ok` arm of
//! `GovernedTransaction::commit`. So:
//!
//! * a rolled-back transaction drops its pending notices with itself;
//! * a failed `COMMIT` returns `Err` and yields no carrier at all;
//! * there is no order of calls a consumer can write that publishes first.
//!
//! That makes "commit before notify" a property of the TYPE rather than of a
//! line's position, which is why its attribution is two compile-fail probes
//! (`tests/probes/02_*`, `03_*`) plus the runtime witnesses that a failed or
//! abandoned commit yields nothing. `F3B2_RESIDUALS.md` records that accounting
//! rather than claiming a source-order mutation that does not exist.
//!
//! # Advisory, bounded, and lossy by contract
//!
//! A notice carries IDENTITY only — event id, aggregate, sequence, kind and the
//! writer epoch that minted it. It never carries the fact's data. A consumer
//! that wants the payload re-reads durable truth at that exact coordinate, which
//! is what keeps the live path from becoming a projection source (ratified:
//! "live notification is bounded advisory state, never a projection source").
//!
//! The per-transaction buffer is bounded at [`MAX_PENDING_NOTICES`]. Overflow
//! DROPS the excess and counts it; it never fails the append, because an
//! advisory concern may not retroactively fail durable progress. Consumers
//! converge from their own cursor over durable truth, so a dropped notice costs
//! latency, never correctness.
//!
//! # What this module does not own
//!
//! There is no concrete [`Notifier`] here. A bounded lossy fanout with
//! per-subscriber interests, queues and cursor convergence is the event-feed
//! surface (`reconnect-feed`, paused behind this lane) and the R4 API.
//! F3B2 owns the boundary the fanout must attach to, and hands out nothing else.

use grapt_event::{AggregateId, EventKind};
use grapt_types::{EventId, WriterEpoch};

/// The most notices one governed transaction retains before it starts dropping.
///
/// A governed transaction is one domain decision plus the facts it commits: the
/// six consumer services each append one or two events per transaction, so this
/// is roughly thirty times the observed shape. It is a bound on the ADVISORY
/// buffer only — the transaction may still append as many facts as it likes, and
/// every one of them is durable whether or not its notice survived.
///
/// Public because it is a declared resource bound, not a capability: a fanout
/// sizing its own queues, or a witness proving the bound holds, needs the exact
/// number. Knowing it grants nothing.
pub const MAX_PENDING_NOTICES: usize = 64;

/// One committed durable fact, named for advisory delivery.
///
/// Identity only, by contract: no `data` field exists to fill. See the module
/// docs for why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableNotice {
    id: EventId,
    aggregate: AggregateId,
    seq: u64,
    kind: EventKind,
    writer: WriterEpoch,
}

impl DurableNotice {
    /// Crate-private: a notice is minted by the append path and by nothing else.
    pub(crate) fn new(
        id: EventId,
        aggregate: AggregateId,
        seq: u64,
        kind: EventKind,
        writer: WriterEpoch,
    ) -> Self {
        DurableNotice {
            id,
            aggregate,
            seq,
            kind,
            writer,
        }
    }

    pub fn id(&self) -> &EventId {
        &self.id
    }

    /// Family PLUS identity — the same key the sequence cursor and the election
    /// row use. A subscriber's interest is expressed against this, which is what
    /// keeps delivery interest-bound instead of requiring a global order.
    pub fn aggregate(&self) -> &AggregateId {
        &self.aggregate
    }

    /// The aggregate-scoped sequence. There is no global commit sequence in this
    /// design and this value is not one: two notices from different aggregates
    /// are not ordered by comparing it.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn kind(&self) -> EventKind {
        self.kind
    }

    /// The elected writer epoch the fact was appended under. A consumer that
    /// tracks it observes ownership transfer without reading the election row.
    pub fn writer(&self) -> WriterEpoch {
        self.writer
    }
}

/// A bounded, advisory sink for committed facts.
///
/// [`Self::notify`] returns nothing. That is the enforcement of "the notifier
/// must not backpressure or retroactively fail durable progress": there is no
/// error channel through which a slow or broken subscriber could reach back into
/// an append that has already committed.
pub trait Notifier {
    fn notify(&mut self, notice: &DurableNotice);

    /// Notices the store's bounded buffer could not retain.
    ///
    /// A fanout that tracks it can mark its stream lossy so a subscriber knows
    /// to re-list from its cursor rather than assume continuity. The default is
    /// to ignore it, because losing an advisory notice is legal by contract.
    fn dropped(&mut self, count: u32) {
        let _ = count;
    }
}

/// Notices accumulated inside an open transaction. Never leaves this crate.
#[derive(Debug, Default)]
pub(crate) struct PendingNotices {
    notices: Vec<DurableNotice>,
    dropped: u32,
}

impl PendingNotices {
    /// Retains the notice, or counts it as dropped once the bound is reached.
    ///
    /// The excess is dropped at the NEW end rather than the old: retaining the
    /// earliest facts of a transaction keeps the reported prefix contiguous, and
    /// a consumer that sees a non-zero drop count re-lists the rest from its own
    /// cursor. Neither end is load-bearing — both are lossy — but a witness has
    /// to pin one, so this one is pinned and stated.
    pub(crate) fn push(&mut self, notice: DurableNotice) {
        if self.notices.len() >= MAX_PENDING_NOTICES {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.notices.push(notice);
    }

    /// The ONLY conversion into the published carrier. Crate-private, and its
    /// one caller is `GovernedTransaction::commit` after `COMMIT` returned `Ok`.
    pub(crate) fn into_committed(self) -> CommittedNotices {
        CommittedNotices {
            notices: self.notices,
            dropped: self.dropped,
        }
    }
}

/// Notices whose facts are durable, handed out by a SUCCESSFUL commit.
///
/// Holding one is proof the transaction committed. There is no public
/// constructor, no `Default`, and no other producer in the crate.
///
/// `#[must_use]` so a caller cannot silently drop the carrier: every consumer
/// states its publication posture explicitly, either [`Self::publish`] or
/// [`Self::discard`], and a reviewer can grep for which.
#[derive(Debug)]
#[must_use = "a committed notice is either published to a notifier or explicitly discarded"]
pub struct CommittedNotices {
    notices: Vec<DurableNotice>,
    dropped: u32,
}

impl CommittedNotices {
    /// Hands every retained notice to `notifier`, then reports the drop count.
    ///
    /// Returns nothing: publication cannot fail the durable progress that
    /// already happened.
    pub fn publish(self, notifier: &mut dyn Notifier) {
        for notice in &self.notices {
            notifier.notify(notice);
        }
        if self.dropped > 0 {
            notifier.dropped(self.dropped);
        }
    }

    /// This caller publishes nothing.
    ///
    /// Legal and named rather than implicit: advisory notices are lossy by
    /// contract, so discarding one loses latency and never truth. Compare with a
    /// persistence FAILURE, which is typed and may never be logged and ignored.
    pub fn discard(self) {}

    pub fn len(&self) -> usize {
        self.notices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notices.is_empty()
    }

    /// How many notices the bounded buffer could not retain.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    pub fn iter(&self) -> impl Iterator<Item = &DurableNotice> {
        self.notices.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn notice(id: &str, seq: u64) -> Result<DurableNotice, Box<dyn std::error::Error>> {
        Ok(DurableNotice::new(
            id.parse()?,
            AggregateId::Session("ses_n1".parse()?),
            seq,
            EventKind::SessionCreatedV1,
            WriterEpoch::FIRST,
        ))
    }

    #[test]
    fn the_buffer_is_bounded_and_reports_exactly_what_it_dropped() -> TestResult {
        let mut pending = PendingNotices::default();
        let overflow = 6_u64;
        let total = MAX_PENDING_NOTICES as u64 + overflow;
        for seq in 0..total {
            pending.push(notice(&format!("evt_n{seq}"), seq)?);
        }
        let committed = pending.into_committed();
        assert_eq!(
            committed.len(),
            MAX_PENDING_NOTICES,
            "the buffer must stop at its bound"
        );
        assert_eq!(
            committed.dropped(),
            overflow as u32,
            "every dropped notice must be counted, not merely lost"
        );
        // The RETAINED end is pinned too. Asserting only the two counts above
        // would hold just as well if the buffer had kept the last 64 and thrown
        // away the first six.
        let first = committed.iter().next().ok_or("a retained notice")?;
        let last = committed.iter().last().ok_or("a retained notice")?;
        assert_eq!(first.seq(), 0);
        assert_eq!(last.seq(), MAX_PENDING_NOTICES as u64 - 1);
        Ok(())
    }

    #[test]
    fn publish_reports_the_drop_count_once_and_only_when_lossy() -> TestResult {
        #[derive(Default)]
        struct Recorder {
            seen: Vec<u64>,
            dropped_calls: Vec<u32>,
        }
        impl Notifier for Recorder {
            fn notify(&mut self, notice: &DurableNotice) {
                self.seen.push(notice.seq());
            }
            fn dropped(&mut self, count: u32) {
                self.dropped_calls.push(count);
            }
        }

        // Lossless: the drop hook is not called at all, so a fanout does not mark
        // a complete stream lossy.
        let mut lossless = PendingNotices::default();
        lossless.push(notice("evt_n0", 0)?);
        let mut recorder = Recorder::default();
        lossless.into_committed().publish(&mut recorder);
        assert_eq!(recorder.seen, vec![0]);
        assert!(
            recorder.dropped_calls.is_empty(),
            "a lossless publication must not report a drop"
        );

        // Lossy: exactly one report, carrying the exact count.
        let mut lossy = PendingNotices::default();
        for seq in 0..(MAX_PENDING_NOTICES as u64 + 2) {
            lossy.push(notice(&format!("evt_m{seq}"), seq)?);
        }
        let mut recorder = Recorder::default();
        lossy.into_committed().publish(&mut recorder);
        assert_eq!(recorder.seen.len(), MAX_PENDING_NOTICES);
        assert_eq!(recorder.dropped_calls, vec![2]);
        Ok(())
    }
}
