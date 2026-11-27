//! One lossy diagnostic queue shared by the existing execution owners.
use super::*;
use grapt_types::timing::{TimingClass, TimingContext, TimingObserver, TimingSample, TimingScope};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TryRecvError};
mod cache;
use crate::diagnostics::LossyOutput;
const LOSS_FIELD: &str = "GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least";
const CAPACITY: usize = 128;
#[derive(Debug)]
pub(super) struct TailObserver {
    clock: Arc<dyn ClockAuthority + Send + Sync>,
    sender: SyncSender<TimingSample>,
    lost: Arc<AtomicU64>,
}
#[derive(Debug)]
pub(super) struct TailSamples {
    receiver: Receiver<TimingSample>,
    lost: Arc<AtomicU64>,
    output: LossyOutput,
}
impl TailObserver {
    pub(super) fn bounded(
        clock: Arc<dyn ClockAuthority + Send + Sync>,
    ) -> (Arc<Self>, TailSamples) {
        Self::open(clock, false)
    }

    pub(super) fn volatile(
        clock: Arc<dyn ClockAuthority + Send + Sync>,
    ) -> (Arc<Self>, TailSamples) {
        Self::open(clock, true)
    }

    fn open(
        clock: Arc<dyn ClockAuthority + Send + Sync>,
        ephemeral: bool,
    ) -> (Arc<Self>, TailSamples) {
        let (sender, receiver) = sync_channel(CAPACITY);
        let lost = Arc::new(AtomicU64::new(0));
        (
            Arc::new(Self {
                clock,
                sender,
                lost: Arc::clone(&lost),
            }),
            TailSamples {
                receiver,
                output: if ephemeral {
                    LossyOutput::disabled(Arc::clone(&lost))
                } else {
                    LossyOutput::stderr(Arc::clone(&lost), LOSS_FIELD)
                },
                lost,
            },
        )
    }
}
impl TimingObserver for TailObserver {
    fn now(&self) -> grapt_types::Timestamp {
        self.clock.now()
    }
    fn record(&self, sample: TimingSample) {
        if self.sender.try_send(sample).is_err() {
            // Saturating: a long-lived process never wraps a loss back to zero.
            let _ = self
                .lost
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                    Some(old.saturating_add(1))
                });
        }
    }
}
impl LiveExecutor {
    pub(super) fn report_dispatch_samples(&self) {
        let samples = match self.dispatch_samples.lock() {
            Ok(samples) => samples,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Global diagnostic queue, attributed by exact attempt identity rather
        // than by whichever concurrent execution happens to drain it.
        for _ in 0..grapt_provider::DISPATCH_SAMPLE_CAPACITY {
            match samples.try_next() {
                Ok(sample) => {
                    self.provider_errors.record_usage(&sample);
                    self.location
                        .calibrations
                        .observe_request(&sample, samples.lost_at_least());
                    let metadata = sample.session.as_ref().and_then(|session| {
                        self.location
                            .calibrations
                            .request_observation(session, &sample.attempt)
                    });
                    let cache = cache::record(sample.session.as_ref(), metadata.as_ref());
                    self.dispatch_line(format!(
                    "GRAPT_PROVIDER_DISPATCH attempt={} at_ms={} response_head_at_ms={} first_stream_byte_at_ms={} http_status={} request_body_bytes={} tools_bytes={} instructions_bytes={} cache_request={}\n",
                    sample.attempt.as_str(),
                    sample.at_ms,
                    serde_json::json!(sample.response_head_at_ms),
                    serde_json::json!(sample.first_stream_byte_at_ms),
                    serde_json::json!(sample.http_status),
                    serde_json::json!(sample.request.as_ref().map(|request| request.body_bytes)),
                    serde_json::json!(sample.request.as_ref().map(|request| request.tools_bytes)),
                    serde_json::json!(sample.request.as_ref().map(|request| request.instructions_bytes)),
                    cache
                ));
                }
                Err(_) => break,
            }
        }
        // Refusals go to the service's own diagnostic file, never stderr.
        for _ in 0..grapt_provider::DISPATCH_SAMPLE_CAPACITY {
            match samples.try_next_refusal() {
                Ok(refusal) => self.provider_errors.record(&refusal),
                Err(_) => break,
            }
        }
        let loss = samples.lost_at_least();
        self.location.calibrations.note_dispatch_loss(loss);
        if loss != 0 {
            self.dispatch_line(format!(
                "GRAPT_PROVIDER_DISPATCH_LOSS count_at_least={loss}\n"
            ));
        }
    }

    fn dispatch_line(&self, line: String) {
        let samples = match self.tail_samples.lock() {
            Ok(samples) => samples,
            Err(poisoned) => poisoned.into_inner(),
        };
        // The HTTP history reader must never wait for a stopped stderr reader.
        samples.output.send(line);
    }

    pub(in crate::dogfood) fn cache_request_metadata(
        &self,
        session: &SessionId,
        attempt: &PhysicalAttemptId,
    ) -> Option<serde_json::Value> {
        // History hydration also runs between tool continuations, before the
        // containing execution ends. Drain the same bounded diagnostic feed.
        self.report_dispatch_samples();
        self.location
            .calibrations
            .request_observation(session, attempt)
    }

    pub(super) fn report_tail_samples(&self) {
        let samples = match self.tail_samples.lock() {
            Ok(samples) => samples,
            Err(poisoned) => poisoned.into_inner(),
        };
        samples.report();
    }
}
impl TailSamples {
    fn report(&self) {
        for _ in 0..CAPACITY {
            match self.receiver.try_recv() {
                Ok(sample) => self.output.send(format!(
                    "GRAPT_PROVIDER_DISPATCH tail_stage={:?} class={:?} session={} attempt={} start_ms={} end_ms={} clock_order={}\n",
                    sample.stage, sample.scope.class,
                    sample.scope.session.as_ref().map_or("none", SessionId::as_str),
                    sample.scope.attempt.as_ref().map_or("none", PhysicalAttemptId::as_str),
                    sample.start.as_millis(), sample.end.as_millis(), if sample.end >= sample.start { "ordered" } else { "inverted" },
                )),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
        }
        let lost = self.lost.load(Ordering::Relaxed);
        if lost != 0 {
            self.output.send(format!(
                "GRAPT_PROVIDER_TAIL_LOSS tail_count_at_least={lost}\n"
            ));
        }
    }
}
pub(super) fn session_context(
    observer: &Arc<dyn TimingObserver>,
    session: &SessionId,
    class: TimingClass,
) -> TimingContext {
    TimingContext {
        observer: Arc::clone(observer),
        scope: TimingScope {
            session: Some(session.clone()),
            attempt: None,
            class,
        },
    }
}
#[cfg(test)]
mod tests;
