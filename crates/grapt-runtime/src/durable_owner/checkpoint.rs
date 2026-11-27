//! Durable checkpoints of an open text block (CLAIM-2).
//!
//! A text block's bytes are held in memory until it closes. A process that
//! dies mid-stream would lose text the user already saw, so while a text block
//! is open its text so far is checkpointed at a bounded cadence and process-
//! loss recovery promotes the newest checkpoint.
//!
//! # Cadence and the loss window
//!
//! Checked as each delta is accumulated; there is no timer. A checkpoint is
//! due when at least [`CHECKPOINT_INTERVAL_MS`] passed since the previous one
//! (or since the block opened) AND the bytes added since are at least
//! `1/`[`CHECKPOINT_GROWTH_DIVISOR`] of the block so far. Each checkpoint is
//! cumulative: one artifact capture of the text so far plus one stream
//! (`synchronous=NORMAL`) commit, never an fsync per delta. The growth rule
//! keeps the rewritten bytes within about 17 times the block's final size and
//! the count at a few hundred for a block at [`MAX_BLOCK_CAPTURE_BYTES`].
//!
//! On a process crash the text lost is what arrived after the newest
//! checkpoint: at most the larger of the last [`CHECKPOINT_INTERVAL_MS`] of
//! streaming and 1/16 of the block (64 KiB at the block bound). A block that
//! closes within the first interval writes no checkpoint. Only text blocks
//! are checkpointed: reasoning is not the answer, and finished reasoning is
//! held and ordered by its own rule.

use super::*;
use grapt_event::session_message::checkpoint::ContentCheckpointedFact;

/// The least time between two checkpoints of one block.
pub const CHECKPOINT_INTERVAL_MS: i64 = 1_000;

/// A checkpoint also waits until the block grew by this fraction of itself.
pub const CHECKPOINT_GROWTH_DIVISOR: usize = 16;

/// What the newest checkpoint of an open block covers.
#[derive(Debug, Clone, Copy)]
pub(super) struct Checkpointed {
    /// Bytes of the block's text the newest checkpoint holds.
    bytes: usize,
    /// When it committed, or when the block opened.
    at_ms: i64,
}

impl Checkpointed {
    pub(super) fn opened_at(at_ms: i64) -> Self {
        Self { bytes: 0, at_ms }
    }

    /// Whether a block now holding `held` bytes is due at `now_ms`.
    pub(super) fn due(&self, held: usize, now_ms: i64) -> bool {
        let added = held.saturating_sub(self.bytes);
        added > 0
            && now_ms.saturating_sub(self.at_ms) >= CHECKPOINT_INTERVAL_MS
            && added >= held / CHECKPOINT_GROWTH_DIVISOR
    }
}

impl<A: ArtifactStore, S: CommittedFactSink> DurableAttemptOwner<'_, A, S> {
    /// Checkpoint the open text block `block` when its cadence says so.
    pub(super) fn checkpoint_if_due(&mut self, block: u32) -> Result<(), OwnerError> {
        let now = self.now_ms();
        let Some((_, open)) = self
            .blocks
            .iter()
            .find(|(held, open)| *held == block && open.kind == ContentKind::Text)
        else {
            return Ok(());
        };
        if !open.checkpoint.due(open.text.len(), now) {
            return Ok(());
        }
        let held = open.text.len();
        let bound = CaptureBound::new(MAX_BLOCK_CAPTURE_BYTES, BLOCK_CAPTURE_BUFFER_BYTES)
            .ok_or_else(|| OwnerError {
                reason: "invalid block capture bound".into(),
            })?;
        let captured = self
            .artifacts
            .capture(
                open.text.as_bytes(),
                bound,
                ArtifactKind::CapturedOutput,
                Sensitivity::Ordinary,
            )
            .map_err(|source| OwnerError {
                reason: format!("checkpoint capture failed: {source}"),
            })?;
        let attempt = self.attempt.clone().ok_or_else(|| OwnerError {
            reason: "a checkpoint arrived before any attempt opened".into(),
        })?;
        let message = self.message.clone().ok_or_else(|| OwnerError {
            reason: "a checkpoint arrived before its assistant opening committed".into(),
        })?;
        let fact =
            ContentCheckpointedFact::new(attempt, message, block, captured.reference().clone());
        let id = self.event_id()?;
        {
            let mut guard = self.store.lock();
            let writer = self.writer(&guard)?;
            let tx = guard.stream_transaction().map_err(|source| OwnerError {
                reason: source.to_string(),
            })?;
            grapt_store::session_message::record_checkpointed(
                &tx,
                &self.session,
                &id,
                writer,
                now,
                &fact,
                &mut [],
            )
            .map_err(write)?;
            tx.commit()
                .map_err(|source| OwnerError {
                    reason: source.to_string(),
                })?
                .discard();
        }
        if let Some((_, open)) = self.blocks.iter_mut().find(|(held, _)| *held == block) {
            open.checkpoint = Checkpointed {
                bytes: held,
                at_ms: now,
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checkpoint_waits_for_the_interval_and_for_growth() {
        let opened = Checkpointed::opened_at(10_000);
        assert!(!opened.due(0, 20_000), "nothing to checkpoint");
        assert!(!opened.due(5, 10_999), "the interval has not passed");
        assert!(opened.due(5, 11_000));
        let after = Checkpointed {
            bytes: 1_600,
            at_ms: 11_000,
        };
        assert!(!after.due(1_600, 30_000), "no new bytes");
        // 1/16 of 1_699 is 106: 99 new bytes are not enough, 107 are.
        assert!(!after.due(1_699, 30_000));
        assert!(after.due(1_707, 30_000));
        assert!(!after.due(1_707, 11_999), "growth alone is not enough");
    }

    /// The cumulative rewrite stays bounded at the block bound: count and
    /// bytes, for a block that grows one byte per interval and checkpoints at
    /// every chance the cadence gives it.
    #[test]
    fn the_cadence_bounds_checkpoints_and_rewritten_bytes_at_the_block_bound() {
        let bound = usize::try_from(MAX_BLOCK_CAPTURE_BYTES).unwrap_or(usize::MAX);
        let mut last = Checkpointed::opened_at(0);
        let (mut count, mut rewritten) = (0usize, 0usize);
        let mut held = 0usize;
        let mut now = 0i64;
        while held < bound {
            held = (held + 1 + held / 64).min(bound);
            now += CHECKPOINT_INTERVAL_MS;
            if last.due(held, now) {
                count += 1;
                rewritten += held;
                last = Checkpointed {
                    bytes: held,
                    at_ms: now,
                };
            }
        }
        assert!(count <= 400, "{count} checkpoints for one block");
        assert!(rewritten <= 17 * bound, "{rewritten} bytes rewritten");
    }
}
