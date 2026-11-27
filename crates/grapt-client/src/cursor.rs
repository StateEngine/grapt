//! Durable convergence: what this client trusts, and what it merely decorates.
//!
//! ## The rule the whole module exists to enforce
//!
//! > Live delivery remains advisory; reconnect must converge from durable
//! > truth.
//!
//! The frozen record makes that operational. `v2.event.subscribe` is volatile
//! SSE with no replay cursor, and `v2.message.list` is the durable re-list.
//! Nine members carry `{aggregateID, seq, version: 1}`; five carry none. So
//! the stream is two interleaved things at once — an ordered durable log and
//! an unordered advisory decoration — and the whole correctness question is
//! whether the client can tell them apart at every instant.
//!
//! ## A cold cursor is NOT live
//!
//! [`DurableCursor::new`] starts in [`StreamState::Reconciling`], not
//! [`StreamState::Live`]. That is deliberate and it is the single most
//! important decision here.
//!
//! A client that started live and began rendering the first frames it saw
//! would be rendering a suffix — everything before it connected is missing,
//! and nothing in the stream says so. It would look perfectly healthy. The
//! frozen contract offers no replay cursor on the subscription precisely
//! because the stream is not the history; the re-list is. So this cursor
//! refuses to claim liveness until a durable re-list has established a
//! baseline, and any gap afterwards returns it to that same state.
//!
//! The consequence is the property the restart law wants: the rendered
//! transcript is always a function of durable truth, with live frames only
//! ever accelerating what a re-list would have produced anyway. Losing every
//! live frame costs latency, never content.
//!
//! ## Why duplicates are ordinary rather than exceptional
//!
//! Reconciliation re-reads durable history the client has often already seen.
//! If re-application were destructive, every reconnect would duplicate the
//! transcript — which is exactly the defect the frozen corpus names in
//! `reconnect-cursor-convergence-replay`. So [`Admission::Duplicate`] is a
//! first-class, expected outcome, and applying the same durable event twice is
//! required to be a no-op rather than merely unlikely.

use std::collections::BTreeMap;

use crate::contract::{EventEnvelope, EventMember};
use crate::wire::SessionWireId;

/// Why the client is reconciling rather than live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GapReason {
    /// No durable baseline has been established yet.
    ///
    /// The cold start. A client in this state has NOT missed anything — it has
    /// simply not yet earned the right to claim it has everything.
    Cold,
    /// The subscription ended. Graceful and abrupt ends are the same fact.
    StreamClosed,
    /// A durable frame arrived for an aggregate with NO baseline.
    ///
    /// Not loss: the client simply has no snapshot for this aggregate, so it
    /// cannot know what precedes the frame. A sparse first event cannot prove
    /// snapshot completeness, so the client hydrates rather than adopting the
    /// position it happens to have seen.
    ///
    /// `aggregate` is the TYPED session identity, not a string copy of it. A
    /// gap reason is rendered to the user and compared in witnesses, so a
    /// string here would be a second spelling of an identity the cursor already
    /// holds typed — and two spellings is the defect.
    ///
    /// This REPLACED a `SequenceGap` variant that fired whenever a durable
    /// sequence was not exactly one greater than the last. That invariant was
    /// invented here and the contract contradicts it outright: within-aggregate
    /// sequences are monotonic-WITH-GAPS and must never be assumed contiguous
    /// (SECODE_RUST_RECONSTRUCTION_DISPOSITION_2026-08-07.md:175-179), `Seq` is
    /// a position in the aggregate's DURABLE LOG rather than a delivery counter
    /// (packages/schema/src/event.ts:15-19), and `/api/event` is volatile by
    /// contract with overflow and disconnection losing events outright
    /// (packages/protocol/src/groups/event.ts:44-47). Against a real product
    /// one prompt delivered seq 1,2,3,4,5,6,9 — perfectly legal — and the old
    /// rule called seq 9 a gap, dropped out of live, and swallowed the
    /// `session.execution.succeeded` frame it was waiting for. Real loss is
    /// signalled by the TRANSPORT, and those signals are kept: see
    /// [`Self::StreamClosed`] and [`Self::BufferOverflow`].
    NoBaseline {
        aggregate: SessionWireId,
        observed: u64,
    },
    /// A bound refused a frame, so the stream is no longer complete.
    ///
    /// A dropped frame and a dropped connection have identical consequences
    /// for correctness, so they take identical recovery.
    BufferOverflow,
}

/// Whether the client currently holds a complete durable prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamState {
    /// A durable baseline is established and no gap has been observed.
    Live,
    /// Durable truth must be re-read before the transcript can be trusted.
    Reconciling { reason: GapReason },
}

impl StreamState {
    pub fn is_live(&self) -> bool {
        matches!(self, StreamState::Live)
    }
}

/// What the cursor decided about one decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Apply it to the projection and advance any durable watermark.
    Apply,
    /// Already applied. Re-application must be a no-op.
    Duplicate,
    /// Advisory decoration: apply for latency, but it carries no durable
    /// position and losing it costs nothing.
    Advisory,
    /// A gap was observed. The frame is NOT applied and the client must
    /// reconcile.
    Gap { reason: GapReason },
    /// The client is already reconciling; live frames are held off until a
    /// baseline exists.
    Deferred,
}

/// Per-aggregate durable watermarks plus the stream's health.
#[derive(Debug, Clone)]
pub struct DurableCursor {
    /// Per-session durable watermarks, keyed by the TYPED identity.
    ///
    /// Keying on `String` was review blocker 1 at this ownership point: two
    /// spellings of one session would have been two independent watermarks,
    /// and each would have reported the other's frames as a sequence gap
    /// forever. A `SessionWireId` key cannot hold two spellings, because there
    /// is only one value that parses to a given identity.
    applied: BTreeMap<SessionWireId, u64>,
    state: StreamState,
    /// Rows in the adopted baseline this client decoded but cannot draw.
    ///
    /// Held beside the state rather than inside it, because it is not a gap and
    /// must never be able to make one: liveness reads `state`, the screen reads
    /// this.
    unsupported_rows: usize,
}

impl Default for DurableCursor {
    fn default() -> Self {
        Self::new()
    }
}

impl DurableCursor {
    /// A cold cursor, which is reconciling rather than live. See the module
    /// documentation for why this is not an implementation detail.
    pub fn new() -> Self {
        Self {
            unsupported_rows: 0,
            applied: BTreeMap::new(),
            state: StreamState::Reconciling {
                reason: GapReason::Cold,
            },
        }
    }

    pub fn state(&self) -> &StreamState {
        &self.state
    }

    /// The highest contiguous durable sequence applied for one aggregate.
    pub fn watermark(&self, aggregate: &SessionWireId) -> Option<u64> {
        self.applied.get(aggregate).copied()
    }

    /// Record that a COMPLETE history contained rows this client cannot
    /// present.
    ///
    /// Deliberately NOT a gap and deliberately not a reason to stop being live.
    /// The rows were read; the client simply has no drawing for them yet, and
    /// the screen says how many. Conflating this with missing history is the
    /// defect this method exists to make impossible to write again.
    pub fn presentation_unsupported(&mut self, rows: usize) {
        self.unsupported_rows = rows;
    }

    /// How many rows of the current baseline this client cannot present.
    pub fn unsupported_rows(&self) -> usize {
        self.unsupported_rows
    }

    /// Adopt a durable baseline established by a readable newest-page re-list.
    ///
    /// This is the ONLY route from reconciling to live. A client cannot decide
    /// on its own that a gap has healed; it has to go and read. The read has
    /// to establish the TAIL — the newest page — and earlier history it has not
    /// covered is reported beside this state, never as a gap.
    pub fn adopt_baseline(&mut self, aggregate: &SessionWireId, seq: u64) {
        let entry = self.applied.entry(aggregate.clone()).or_insert(seq);
        // A re-list can only move a watermark forward. If a live frame already
        // advanced past the re-listed point, the re-list is the older view and
        // must not rewind the client.
        if seq > *entry {
            *entry = seq;
        }
        self.state = StreamState::Live;
    }

    /// Record that the subscription ended.
    pub fn stream_closed(&mut self) {
        self.state = StreamState::Reconciling {
            reason: GapReason::StreamClosed,
        };
    }

    /// A resume the service certified continues the stream a cut closed, at
    /// exactly the frame this client last held: nothing was missed, so the
    /// watermarks stand and no baseline is needed. Only a closed stream
    /// resumes; every other reason still needs a re-read. True when live.
    pub fn resume_certified(&mut self) -> bool {
        let closed = matches!(
            self.state,
            StreamState::Reconciling {
                reason: GapReason::StreamClosed
            }
        );
        if closed {
            self.state = StreamState::Live;
        }
        closed
    }

    /// Record that a bound refused a frame.
    pub fn buffer_overflowed(&mut self) {
        self.state = StreamState::Reconciling {
            reason: GapReason::BufferOverflow,
        };
    }

    /// Decide what to do with one decoded frame.
    ///
    /// Live members never advance a watermark and never heal a gap. That
    /// asymmetry is the point: a transport that stamped `durable` onto a live
    /// frame could otherwise walk the watermark forward over history the
    /// client never actually received.
    pub fn admit(&mut self, envelope: &EventEnvelope) -> Admission {
        let Some(durable) = envelope.durable.as_ref() else {
            // Advisory. `server.connected` is meaningful even while
            // reconciling — it is how the client learns a subscription is
            // established at all — but it still carries no durable position.
            //
            // The quiescence signals are the same shape of fact: no durable position to
            // corrupt a baseline with, and no durable record carries them, so deferring
            // them while reconciling is not "the re-list will supersede" — it is "the
            // fact is lost", because the re-subscription that follows every stream cut
            // opens exactly inside the reconciling window that cut opened, and its
            // hydration is the only recovery path a dropped live edge leaves behind.
            return match (&self.state, envelope.member) {
                (
                    StreamState::Reconciling { .. },
                    Some(
                        EventMember::ServerConnected
                        | EventMember::SessionStatus
                        | EventMember::MailAttention
                        | EventMember::ContinuityAttention
                        | EventMember::SessionIdle,
                    ),
                ) => Admission::Advisory,
                (StreamState::Reconciling { .. }, _) => Admission::Deferred,
                (StreamState::Live, _) => Admission::Advisory,
            };
        };

        if !self.state.is_live() {
            // Durable frames observed while reconciling are not applied. The
            // re-list is authoritative and is about to supersede them;
            // applying one here would establish a baseline from a suffix,
            // which is the exact defect a cold start avoids.
            return Admission::Deferred;
        }

        match self.applied.get(&durable.aggregate_id).copied() {
            None => {
                // Live, but this aggregate has no baseline: the re-list did not
                // cover it, so the client does not know what precedes this
                // frame. It hydrates rather than adopting the position it
                // happens to have seen — an unknown position is not zero, and
                // one sparse event cannot prove a snapshot is complete.
                let reason = GapReason::NoBaseline {
                    aggregate: durable.aggregate_id.clone(),
                    observed: durable.seq,
                };
                self.state = StreamState::Reconciling {
                    reason: reason.clone(),
                };
                Admission::Gap { reason }
            }
            // MONOTONIC, NOT CONTIGUOUS. A sequence at or below the watermark
            // has already been folded in; anything above it is new and is
            // applied, however far above. Requiring exactly one more was an
            // invented invariant that the contract contradicts, and it cost
            // this client every live execution terminal it ever received.
            Some(applied) if durable.seq <= applied => Admission::Duplicate,
            Some(_) => {
                self.applied
                    .insert(durable.aggregate_id.clone(), durable.seq);
                Admission::Apply
            }
        }
    }
}

#[cfg(test)]
mod tests;
