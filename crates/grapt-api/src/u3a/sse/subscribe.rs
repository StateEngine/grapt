//! Subscribing one HTTP window, fresh or resumed (B3 S3b).
use super::*;
use grapt_stream::replay::MAX_BOOTSTRAP_BYTES;

impl DogfoodEventHub {
    /// [`Self::subscribe_window_at`] in either subscriber class. A positions
    /// window gets an `id:` line on every frame, starting with its first.
    ///
    /// A positions window that names its last frame (`resume`) is resumed when
    /// the replay ring certifies it: its first frame echoes that id, and the
    /// frames it missed follow, with no hydration, exactly as an
    /// uninterrupted subscriber received them. Nothing is queued before that
    /// is decided, so a refused resume is subscribed afresh, unchanged.
    ///
    /// `retained` are Sessions this window showed before; a positions window
    /// keeps a claim on each for as long as this stream lives. Any other
    /// window holds none: the query refuses `retain=` without `positions=1`.
    pub(super) fn subscribe_window(
        &self,
        interest: query::EventLocationQuery,
        approver: Option<Approver>,
        session: Option<grapt_types::SessionId>,
        retained: Vec<grapt_types::SessionId>,
        positions: bool,
        resume: Option<&str>,
    ) -> Result<SseBody, SseError> {
        let mut subscribers = self
            .subscribers
            .lock()
            .map_err(|_| SseError::RegistryUnavailable)?;
        subscribers.retain(|subscriber| !subscriber.sink.is_closed());
        if subscribers.len() >= self.limits.subscribers() {
            return Err(SseError::SubscriberBound);
        }
        let (sender, receiver) = mpsc::channel(self.limits.pending_events());
        let resumed = match (positions, resume) {
            (true, Some(id)) => {
                let registry = &mut *subscribers;
                registry
                    .replay
                    .opening_batch(
                        id,
                        (registry.positions.epoch(), registry.positions.seq()),
                        &self.connected,
                        (self.limits.pending_events(), MAX_BOOTSTRAP_BYTES),
                        |location| v2_interested(&interest, location),
                    )
                    .ok()
            }
            _ => None,
        };
        if let Some(batch) = resumed {
            for frame in batch {
                sender.try_send(frame).map_err(|_| SseError::Encode)?;
            }
        } else {
            let mut stamp = |frame: Bytes| {
                if positions {
                    subscribers.positions.stamp(&frame)
                } else {
                    Ok(frame)
                }
            };
            sender
                .try_send(stamp(self.connected.clone())?)
                .map_err(|_| SseError::Encode)?;
            // An HTTP client keeps a partial snapshot: hydration is courtesy
            // there, and its live stream stays usable.
            let _partial = self.queue_hydration(&interest, &mut |frame| {
                stamp(frame).is_ok_and(|frame| sender.try_send(frame).is_ok())
            });
        }
        let overflowed = Arc::new(AtomicBool::new(false));
        // A window on a Session the registry cannot track is refused, not
        // left unclaimed: unclaimed, its turn would outlive the window.
        let mut windows = match &session {
            Some(shown) => vec![self
                .windows
                .attach(shown)
                .ok_or(SseError::SubscriberBound)?],
            None => Vec::new(),
        };
        windows.extend(
            retained
                .iter()
                .filter(|_| positions)
                .filter(|kept| Some(*kept) != session.as_ref())
                .filter_map(|kept| self.windows.retain(kept)),
        );
        let approval_changes = approver.as_ref().map(|_| ApprovalDrop {
            subscribers: Arc::clone(&self.subscribers),
            changes: Arc::clone(&self.approval_changes),
        });
        subscribers.push(SubscriberSender {
            approver,
            sink: SubscriberSink::Http {
                sender,
                overflowed: Arc::clone(&overflowed),
            },
            interest: SubscriberInterest::V2(interest),
            positions,
        });
        subscribers.refresh();
        self.approvals_changed();
        Ok(SseBody {
            approval_changes,
            receiver,
            overflowed,
            terminated: false,
            _windows: windows,
        })
    }

    /// Replay-ring diagnostics, without the subscriber lock a publish holds across a commit.
    pub fn replay_counts(&self) -> grapt_stream::replay::ReplayCounts {
        self.replay_counts.snapshot()
    }
}

/// A boundary no resume may cross, at a fresh position. Exhausted positions
/// refuse every cursor.
pub(super) fn replay_barrier(registry: &mut SubscriberRegistry) {
    let seq = registry.positions.advance().unwrap_or(u64::MAX);
    registry.replay.barrier(seq);
}

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
