//! The hub's stream positions (`id: <epoch>-<seq>`) for the opt-in positions
//! subscriber class. The API encodes the frames this stamps; legacy
//! subscribers never see a position.
use crate::limits::{SseError, MAX_SSE_FRAME_BYTES};
use bytes::Bytes;
use std::sync::atomic::{AtomicU64, Ordering};

/// Room a positioned frame takes past the legacy bounds, once for its JSON (a
/// delta's `from`) and once for its `id:` line, which is never longer.
pub const POSITION_HEADROOM_BYTES: usize = 64;
/// The largest positioned frame a client must accept, framing included.
pub const MAX_POSITIONED_FRAME_BYTES: usize = MAX_SSE_FRAME_BYTES + 2 * POSITION_HEADROOM_BYTES;
static NONCE: AtomicU64 = AtomicU64::new(0);

/// One hub's stream positions: an epoch that changes with every hub (so with
/// every restart) and a sequence advanced under the subscriber lock.
#[derive(Debug)]
pub struct StreamPositions {
    epoch: String,
    seq: u64,
}

impl StreamPositions {
    pub fn new() -> Self {
        use std::hash::BuildHasher;
        // The std hasher is seeded from OS randomness per process, so a restart
        // cannot repeat an epoch; the nonce separates hubs. No clock is read.
        let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
        let process = std::collections::hash_map::RandomState::new().hash_one(nonce);
        Self {
            epoch: format!("{process:016x}x{nonce}"),
            seq: 0,
        }
    }

    /// Prefix one frame with the next position; exhaustion refuses.
    pub fn stamp(&mut self, frame: &[u8]) -> Result<Bytes, SseError> {
        self.seq = self.seq.checked_add(1).ok_or(SseError::Encode)?;
        let id = format!("id: {}-{}\n", self.epoch, self.seq);
        let mut stamped = Vec::with_capacity(id.len().saturating_add(frame.len()));
        stamped.extend_from_slice(id.as_bytes());
        stamped.extend_from_slice(frame);
        Ok(Bytes::from(stamped))
    }

    /// This hub's epoch, which a resync frame names.
    pub fn epoch(&self) -> &str {
        &self.epoch
    }

    /// The newest position stamped.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Take the next position without a frame, for a replay barrier.
    pub fn advance(&mut self) -> Result<u64, SseError> {
        self.seq = self.seq.checked_add(1).ok_or(SseError::Encode)?;
        Ok(self.seq)
    }
}

impl Default for StreamPositions {
    fn default() -> Self {
        Self::new()
    }
}
