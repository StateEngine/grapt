//! Volatile ACP timing, read out through the existing diagnostic writer.
use grapt_types::timing::{TimingClass, TimingContext, TimingObserver, TimingSample, TimingScope};
use grapt_types::{ClockAuthority, SessionId, Timestamp};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc::{sync_channel, Receiver, SyncSender},
    Arc, Mutex,
};
const CAPACITY: usize = 128;
/// The one named CLI monotonic diagnostic adapter.
/// Its origin and readings never enter durable facts, ordering, or OAuth expiry.
#[derive(Debug)]
pub(super) struct DiagnosticClock(std::time::Instant);
impl DiagnosticClock {
    pub(super) fn new() -> Self {
        #[allow(
            clippy::disallowed_methods,
            reason = "single monotonic ACP diagnostic clock construction"
        )]
        let origin = std::time::Instant::now();
        Self(origin)
    }
}
impl ClockAuthority for DiagnosticClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(i64::try_from(self.0.elapsed().as_millis()).unwrap_or(i64::MAX))
    }
}
#[derive(Debug)]
pub(super) struct AcpTiming {
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    sender: SyncSender<TimingSample>,
    samples: Mutex<Receiver<TimingSample>>,
    lost: AtomicU64,
}
impl AcpTiming {
    pub(super) fn bounded(clock: Arc<dyn ClockAuthority + Send + Sync>) -> Arc<Self> {
        let (sender, samples) = sync_channel(CAPACITY);
        Arc::new(Self {
            clock,
            sender,
            samples: Mutex::new(samples),
            lost: AtomicU64::new(0),
        })
    }
    pub(super) fn context(self: &Arc<Self>, session: SessionId) -> TimingContext {
        TimingContext {
            observer: Arc::clone(self) as Arc<dyn TimingObserver>,
            scope: TimingScope {
                session: Some(session),
                attempt: None,
                class: TimingClass::Execution,
            },
        }
    }
    pub(super) fn lost_at_least(&self) -> u64 {
        self.lost.load(Ordering::Relaxed)
    }
    pub(super) fn lose(&self) {
        let _ = self
            .lost
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                Some(old.saturating_add(1))
            });
    }
    /// At most 128 samples. Mutex has only this diagnostic consumer; producers
    /// never acquire it and never wait for a reader or queue capacity.
    pub(super) fn drain(&self) -> (Vec<TimingSample>, u64) {
        let samples = match self.samples.lock() {
            Ok(samples) => samples,
            Err(poisoned) => poisoned.into_inner(),
        };
        (
            samples.try_iter().take(CAPACITY).collect(),
            self.lost.load(Ordering::Relaxed),
        )
    }
}
impl TimingObserver for AcpTiming {
    fn now(&self) -> Timestamp {
        self.clock.now()
    }
    fn record(&self, sample: TimingSample) {
        if self.sender.try_send(sample).is_err() {
            let _ = self
                .lost
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                    Some(old.saturating_add(1))
                });
        }
    }
}
#[cfg(test)]
mod tests;
