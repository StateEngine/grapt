//! The hub's replay ring for resumable positions subscribers (B3 S3b).
//!
//! Every positioned frame the hub offers is kept here, oldest first, within a
//! frame and a byte bound. A reconnecting positions subscriber that names its
//! last frame (`Last-Event-ID: <epoch>-<seq>`) is resumed only when nothing it
//! could have missed is gone: the ring still reaches back to its cursor, no
//! barrier fell after it, and the whole replay fits one opening batch. Anything
//! else is refused, and the caller subscribes it afresh.
//!
//! A barrier is a boundary at which the hub ended every live positions
//! subscriber (an invalidation, a reconcile, a shutdown, or a frame that could
//! not be positioned): no resume may cross it, because no uninterrupted
//! subscriber survived it either.
use bytes::Bytes;
use grapt_types::LocationRef;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Frames the ring holds before evicting the oldest.
pub const MAX_REPLAY_FRAMES: usize = 1_024;
/// Bytes the ring charges before evicting the oldest.
pub const MAX_REPLAY_BYTES: usize = 8 * 1024 * 1024;
/// Bytes one resumed subscriber's opening batch may carry.
pub const MAX_BOOTSTRAP_BYTES: usize = 8 * 1024 * 1024;
/// The longest `Last-Event-ID` a resume may name.
pub const MAX_RESUME_ID_BYTES: usize = 128;

/// One offered frame: its position, the location it was routed by, and the
/// positioned bytes every positions subscriber was offered.
#[derive(Debug, Clone)]
pub struct ReplayEntry {
    pub seq: u64,
    pub location: Option<LocationRef>,
    pub frame: Bytes,
}

impl ReplayEntry {
    /// The frame, the location's heap, and the entry itself.
    fn charge(&self) -> usize {
        let location = self.location.as_ref().map_or(0, |location| {
            location
                .directory()
                .len()
                .saturating_add(location.workspace().map_or(0, |id| id.as_str().len()))
        });
        self.frame
            .len()
            .saturating_add(location)
            .saturating_add(std::mem::size_of::<Self>())
    }
}

/// Why a resume was refused; the subscriber is then subscribed afresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeRefusal {
    /// The id is not `<epoch>-<seq>` within the length bound.
    Malformed,
    /// The id names another hub (a restart).
    Epoch,
    /// A barrier fell after the cursor.
    Barrier,
    /// Frames after the cursor were evicted.
    Evicted,
    /// The cursor is beyond anything this hub stamped.
    Ahead,
    /// The replay exceeds the opening batch's frame or byte bound.
    OverBatch,
}

/// Lifetime counts for this hub's ring; saturation never wraps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ReplayCounts {
    pub resumed: u64,
    pub replayed_frames: u64,
    pub evicted_frames: u64,
    pub barriers: u64,
    pub refused_malformed: u64,
    pub refused_epoch: u64,
    pub refused_barrier: u64,
    pub refused_evicted: u64,
    pub refused_ahead: u64,
    pub refused_over_batch: u64,
}

/// The ring's counts, shared with readers outside the hub's subscriber lock.
/// This lock guards only the counts and is never held across publication, so
/// a health read never waits behind a staged commit.
#[derive(Debug, Default)]
pub struct ReplayCounters(Mutex<ReplayCounts>);

impl ReplayCounters {
    fn update(&self, change: impl FnOnce(&mut ReplayCounts)) {
        change(
            &mut self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
    }

    pub fn snapshot(&self) -> ReplayCounts {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The bounded ring, held under the hub's subscriber lock.
#[derive(Debug)]
pub struct ReplayRing {
    entries: VecDeque<ReplayEntry>,
    bytes: usize,
    max_frames: usize,
    max_bytes: usize,
    /// The newest position no longer held: a cursor before it cannot resume.
    evicted_through: u64,
    /// The newest barrier: a cursor before it cannot resume.
    barrier: u64,
    counts: Arc<ReplayCounters>,
}

impl Default for ReplayRing {
    fn default() -> Self {
        Self::with_bounds(MAX_REPLAY_FRAMES, MAX_REPLAY_BYTES)
    }
}

/// Saturating counter increment.
fn bump(count: &mut u64, by: u64) {
    *count = count.saturating_add(by);
}

impl ReplayRing {
    pub fn with_bounds(max_frames: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_frames,
            max_bytes,
            evicted_through: 0,
            barrier: 0,
            counts: Arc::default(),
        }
    }

    /// Keep one offered frame, evicting the oldest past either bound.
    pub fn push(&mut self, entry: ReplayEntry) {
        self.bytes = self.bytes.saturating_add(entry.charge());
        self.entries.push_back(entry);
        while self.entries.len() > self.max_frames || self.bytes > self.max_bytes {
            let Some(oldest) = self.entries.pop_front() else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(oldest.charge());
            self.evicted_through = oldest.seq;
            self.counts
                .update(|counts| bump(&mut counts.evicted_frames, 1));
        }
    }

    /// A boundary no resume may cross, at the position stamped for it.
    pub fn barrier(&mut self, seq: u64) {
        self.entries.clear();
        self.bytes = 0;
        self.barrier = seq;
        self.evicted_through = seq;
        self.counts.update(|counts| bump(&mut counts.barriers, 1));
    }

    /// The opening batch of a resume, or why it is refused (and counted).
    ///
    /// `id` is the client's `Last-Event-ID`; `epoch` and `current` are this
    /// hub's epoch and newest stamped position. The batch is the connected
    /// frame echoing `id` (the certificate), then every held frame after the
    /// cursor that `interested` admits, as refcount clones. It stops at the
    /// first frame past `max_frames` or `max_bytes` without cloning the rest.
    pub fn opening_batch(
        &mut self,
        id: &str,
        position: (&str, u64),
        connected: &Bytes,
        (max_frames, max_bytes): (usize, usize),
        interested: impl Fn(Option<&LocationRef>) -> bool,
    ) -> Result<Vec<Bytes>, ResumeRefusal> {
        let batch = self.admit(id, position, connected, (max_frames, max_bytes), interested);
        self.counts.update(|counts| match &batch {
            Ok(frames) => {
                bump(&mut counts.resumed, 1);
                let replayed = frames.len().saturating_sub(1);
                bump(
                    &mut counts.replayed_frames,
                    u64::try_from(replayed).unwrap_or(u64::MAX),
                );
            }
            Err(refusal) => bump(
                match refusal {
                    ResumeRefusal::Malformed => &mut counts.refused_malformed,
                    ResumeRefusal::Epoch => &mut counts.refused_epoch,
                    ResumeRefusal::Barrier => &mut counts.refused_barrier,
                    ResumeRefusal::Evicted => &mut counts.refused_evicted,
                    ResumeRefusal::Ahead => &mut counts.refused_ahead,
                    ResumeRefusal::OverBatch => &mut counts.refused_over_batch,
                },
                1,
            ),
        });
        batch
    }

    fn admit(
        &self,
        id: &str,
        (epoch, current): (&str, u64),
        connected: &Bytes,
        (max_frames, max_bytes): (usize, usize),
        interested: impl Fn(Option<&LocationRef>) -> bool,
    ) -> Result<Vec<Bytes>, ResumeRefusal> {
        if id.len() > MAX_RESUME_ID_BYTES || !id.is_ascii() {
            return Err(ResumeRefusal::Malformed);
        }
        let (named, seq) = id.rsplit_once('-').ok_or(ResumeRefusal::Malformed)?;
        if seq.is_empty() || !seq.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(ResumeRefusal::Malformed);
        }
        let cursor: u64 = seq.parse().map_err(|_| ResumeRefusal::Malformed)?;
        if named != epoch {
            return Err(ResumeRefusal::Epoch);
        }
        if cursor < self.barrier {
            return Err(ResumeRefusal::Barrier);
        }
        if cursor < self.evicted_through {
            return Err(ResumeRefusal::Evicted);
        }
        if cursor > current {
            return Err(ResumeRefusal::Ahead);
        }
        let echo = format!("id: {id}\n");
        let mut certificate = Vec::with_capacity(echo.len().saturating_add(connected.len()));
        certificate.extend_from_slice(echo.as_bytes());
        certificate.extend_from_slice(connected);
        let mut bytes = certificate.len();
        let mut batch = vec![Bytes::from(certificate)];
        if batch.len() > max_frames || bytes > max_bytes {
            return Err(ResumeRefusal::OverBatch);
        }
        for entry in self.entries.iter().filter(|entry| entry.seq > cursor) {
            if !interested(entry.location.as_ref()) {
                continue;
            }
            bytes = bytes.saturating_add(entry.frame.len());
            if batch.len() >= max_frames || bytes > max_bytes {
                return Err(ResumeRefusal::OverBatch);
            }
            batch.push(entry.frame.clone());
        }
        Ok(batch)
    }

    pub fn counts(&self) -> ReplayCounts {
        self.counts.snapshot()
    }

    /// The shared counts, for a reader that must not take the subscriber lock.
    pub fn counters(&self) -> Arc<ReplayCounters> {
        Arc::clone(&self.counts)
    }
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
