//! The narrow capture seam — a bounded, non-blocking byte tap.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md` —
//! "Complete bounded capture: every provider/tool byte retained inside the hard
//! I/O bound before unbounded allocation." This crate exposes a narrow capture
//! sink seam without owning its storage; raw-byte storage belongs to its consumer.
//!
//! # The seam now
//!
//! No foreign code runs on the read path. There is no trait for a consumer to
//! implement and therefore nothing this crate calls into. The stream owns the
//! SENDING half of a bounded channel and never does anything but `try_send`,
//! which cannot block and cannot panic.
//!
//! - [`CaptureExchange`] is held by the adapter. Each attempt announces itself
//!   on it once, handing over an [`AttemptCapture`] whose `feed` is that
//!   attempt's own receiver. Attribution is structural: one attempt, one feed,
//!   carrying its `PhysicalAttemptId`.
//! - The stream then `try_send`s raw transport chunks into that feed and sends
//!   exactly one [`CaptureEvent::Finished`] when the attempt settles.
//! - **`Drop` settles.** A stream dropped mid-flight — cancellation, an early
//!   return, a panic elsewhere — settles as [`CaptureOutcome::Incomplete`],
//!   because settlement is a `Drop` obligation rather than a call a caller can
//!   forget.
//! - And even if that final event is itself dropped by a saturated channel, the
//!   consumer still learns the attempt is over: the sender is gone, so its
//!   receiver observes disconnection. **End-of-attempt is not lossy**, only the
//!   bytes are.
//!
//! # Lossy by contract, and it says so
//!
//! `try_send` on a full channel drops the chunk and counts it. A consumer that
//! does not drain loses bytes rather than stalling the provider — the trade the
//! contract demands, since "a full channel cannot cause a background task to
//! mutate a Session, allocate an unbounded queue, or abandon a terminal fact".
//! The byte/count accounting travels in [`CaptureEvent::Finished::loss`], so a
//! consumer is never silently handed a partial capture it believes is whole.
//! Oversized chunks are refused before `to_vec`; admitted chunks are bounded by
//! the injected byte ceiling before the only capture-owned allocation occurs.
//! Attempt identities have their separate public-opener ceiling
//! ([`crate::opener::MAX_PHYSICAL_ATTEMPT_ID_BYTES`]), enforced before capture
//! can clone or announce them.
//!
//! # Still not storage
//!
//! No writer, no path, no digest, no retention policy, no artifact. This crate
//! bounds what it holds and hands bytes across a channel; whether they are ever
//! persisted is the storage consumer's responsibility, not this crate's.

use std::num::NonZeroUsize;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};

use grapt_types::PhysicalAttemptId;

/// Why a captured stream ended.
///
/// Closed. A consumer deciding whether an artifact is whole must be able to
/// tell these apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// The provider produced its own terminal. The captured bytes are a whole
    /// response, minus any the consumer's own backlog dropped.
    Terminal,
    /// The stream failed, was refused, ended without a terminal, or was
    /// CANCELLED. The captured bytes are not a whole response.
    Incomplete,
}

/// One event on an attempt's capture feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEvent {
    /// One transport chunk, exactly as the socket produced it — before UTF-8
    /// decoding and before SSE framing, so a stream this adapter REFUSES is
    /// still captured as it actually arrived.
    Chunk(Vec<u8>),
    /// The attempt settled. At most one per feed, and always last.
    Finished {
        outcome: CaptureOutcome,
        /// Exact count/byte accounting for every chunk capture did not retain.
        loss: CaptureLoss,
    },
}

/// Why capture retained fewer bytes than the transport produced.
///
/// Saturation is loss after a chunk passed the byte ceiling. Oversized is a
/// refusal at the hard boundary, before capture allocates or copies the chunk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureLoss {
    /// Chunks admitted by the byte ceiling but dropped because the feed was full.
    pub saturated_chunks: u32,
    /// Bytes in chunks dropped because the feed was full.
    pub saturated_bytes: usize,
    /// Chunks refused because one value exceeded the injected byte ceiling.
    pub oversized_chunks: u32,
    /// Bytes in chunks refused by the injected byte ceiling.
    pub oversized_bytes: usize,
}

/// One attempt's capture, handed to the consumer when the attempt opens.
#[derive(Debug)]
pub struct AttemptCapture {
    attempt: PhysicalAttemptId,
    feed: Receiver<CaptureEvent>,
}

impl AttemptCapture {
    /// Which attempt these bytes belong to. Structural attribution: a feed
    /// carries exactly one attempt's bytes and says which.
    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    /// The event feed. Draining it is the consumer's job; not draining it costs
    /// bytes, never provider progress.
    ///
    /// A disconnected feed means the attempt is over even if its `Finished`
    /// event was dropped by saturation.
    pub fn feed(&self) -> &Receiver<CaptureEvent> {
        &self.feed
    }
}

/// The adapter-held announcement channel.
///
/// Cloneable and cheap: one exchange serves every attempt an adapter opens.
#[derive(Debug, Clone)]
pub struct CaptureExchange {
    announce: SyncSender<AttemptCapture>,
    /// Per-attempt feed capacity, in chunks.
    feed_capacity: usize,
    /// Maximum bytes one captured chunk may retain. With the count bound this
    /// bounds queued payload bytes; the check runs before the capture copy.
    max_chunk_bytes: usize,
}

/// Create a capture exchange and the receiver a consumer drains.
///
/// `pending_attempts` bounds how many opened-but-unclaimed attempt feeds may
/// queue; every identity entering that production path has already passed
/// [`crate::opener::MAX_PHYSICAL_ATTEMPT_ID_BYTES`]. `feed_capacity` bounds each
/// attempt's own chunk backlog; `max_chunk_bytes` bounds every queued raw chunk
/// before allocation/copy. The three deployment counts are injected with no
/// default, for the same reason the deadlines are: the right numbers are a
/// deployment decision this crate refuses to guess. One feed can retain at
/// most `feed_capacity * max_chunk_bytes` payload bytes, plus at most one
/// bounded transient copy while a non-blocking send discovers saturation.
pub fn capture_exchange(
    pending_attempts: NonZeroUsize,
    feed_capacity: NonZeroUsize,
    max_chunk_bytes: NonZeroUsize,
) -> (CaptureExchange, Receiver<AttemptCapture>) {
    let (announce, attempts) = sync_channel(pending_attempts.get());
    (
        CaptureExchange {
            announce,
            feed_capacity: feed_capacity.get(),
            max_chunk_bytes: max_chunk_bytes.get(),
        },
        attempts,
    )
}

impl CaptureExchange {
    /// Open one attempt's capture, or decline.
    ///
    /// Returns `None` when the announcement channel is full or the consumer is
    /// gone. Declining is deliberate and non-fatal: capture is best-effort, and
    /// an attempt must open whether or not anyone is listening.
    pub(crate) fn open(&self, attempt: &PhysicalAttemptId) -> Option<CaptureTap> {
        let (chunks, feed) = sync_channel(self.feed_capacity);
        let announced = AttemptCapture {
            attempt: attempt.clone(),
            feed,
        };
        match self.announce.try_send(announced) {
            Ok(()) => Some(CaptureTap {
                chunks,
                max_chunk_bytes: self.max_chunk_bytes,
                loss: CaptureLoss::default(),
                settled: false,
            }),
            // Full or disconnected: no feed was handed over, so there is
            // nothing to settle and no tap to hold.
            Err(_) => None,
        }
    }
}

/// The sending half the stream owns, for exactly one attempt.
///
/// Not `Clone`: two taps for one attempt could settle it twice.
#[derive(Debug)]
pub(crate) struct CaptureTap {
    chunks: SyncSender<CaptureEvent>,
    max_chunk_bytes: usize,
    loss: CaptureLoss,
    settled: bool,
}

impl CaptureTap {
    /// Offer one raw transport chunk. Never blocks, never panics.
    ///
    /// A full backlog drops the chunk and counts it, which is the trade the
    /// contract requires: a slow consumer must not be able to stall the
    /// provider reader.
    pub(crate) fn chunk(&mut self, bytes: &[u8]) {
        if self.settled {
            return;
        }
        // This check must precede `to_vec`: one hostile transport chunk may be
        // arbitrarily large, even when the count-bounded feed is already full.
        if bytes.len() > self.max_chunk_bytes {
            self.loss.oversized_chunks = self.loss.oversized_chunks.saturating_add(1);
            self.loss.oversized_bytes = self.loss.oversized_bytes.saturating_add(bytes.len());
            return;
        }
        let byte_count = bytes.len();
        match self.chunks.try_send(CaptureEvent::Chunk(bytes.to_vec())) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.loss.saturated_chunks = self.loss.saturated_chunks.saturating_add(1);
                self.loss.saturated_bytes = self.loss.saturated_bytes.saturating_add(byte_count);
            }
            // The consumer is gone. Stop counting; there is no one to tell.
            Err(TrySendError::Disconnected(_)) => {
                self.settled = true;
            }
        }
    }

    /// Settle this attempt. Idempotent — the second call does nothing.
    pub(crate) fn settle(&mut self, outcome: CaptureOutcome) {
        if self.settled {
            return;
        }
        self.settled = true;
        // try_send, so settlement cannot block either. If it is dropped by a
        // saturated backlog the consumer still learns the attempt ended, because
        // this tap is about to be dropped and its receiver will disconnect.
        let _ = self.chunks.try_send(CaptureEvent::Finished {
            outcome,
            loss: self.loss,
        });
    }
}

impl Drop for CaptureTap {
    /// Settlement is a `Drop` obligation.
    ///
    /// A stream dropped mid-flight is what cancellation looks like from here,
    /// and the first design settled nothing in that case — a consumer waited
    /// forever on an attempt that had been cancelled. There is now no path that
    /// destroys a tap without settling it, and no call a caller can forget.
    fn drop(&mut self) {
        self.settle(CaptureOutcome::Incomplete);
    }
}

#[cfg(test)]
mod tests;
