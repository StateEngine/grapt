//! Context preparation and its joined cleanup precede every physical attempt.
use super::*;

/// Wait, bounded, for the provider transport to hold a place for the next
/// attempt (SC-REL-13). A wait that runs out proceeds: the open then refuses
/// as saturated, in its own words. Returns the interruption that ended it.
pub(super) async fn reserve<S: AttemptSource>(
    source: &mut S,
    cancel: &mut watch::Receiver<Option<InterruptionSource>>,
    interrupt_available: &mut bool,
    bound: Duration,
) -> Option<InterruptionSource> {
    let progress = source.capacity_progress();
    let mut reserving = std::pin::pin!(super::capacity::until_stalled(
        source.reserve_capacity(),
        progress,
        bound
    ));
    loop {
        tokio::select! {
            biased;
            changed = cancel.changed(), if *interrupt_available => {
                if changed.is_err() {
                    *interrupt_available = false;
                } else if let Some(by) = *cancel.borrow() {
                    return Some(by);
                }
            }
            _ = &mut reserving => return None,
        }
    }
}

pub(super) struct PreparationTimeout {
    pub stage: ContextPreparationStage,
    pub refusal: AttemptAdmissionRefusal,
}

impl PreparationTimeout {
    fn deadline(stage: ContextPreparationStage, bound: Duration) -> Self {
        let refusal = match stage {
            ContextPreparationStage::Continuity => AttemptAdmissionRefusal::ContextUnavailable {
                failure: ContextFailure::ContinuityUnavailable,
                continuity: Some(grapt_session::execution::ContinuityFailure::TimedOut {
                    seconds: bound.as_secs().max(1),
                }),
            },
            _ => AttemptAdmissionRefusal::ContextPreparationTimedOut { stage },
        };
        Self { stage, refusal }
    }
}

/// A timeout is recoverable only after the preparation's cleanup is confirmed.
pub(super) type PreparationOutcome<R> = Result<AttemptPreparation<R>, PreparationTimeout>;

pub(super) async fn prepare<S: AttemptSource>(
    source: &mut S,
    cancel: &mut watch::Receiver<Option<InterruptionSource>>,
    interrupt_available: &mut bool,
    step: StepNumber,
    ordinal: u32,
    generation: HistoryGeneration,
    deadline_bound: Duration,
) -> Result<PreparationOutcome<S::Reader>, StepFailure> {
    let mut progress = source.context_preparation_progress();
    let mut stage = progress
        .as_mut()
        .map_or(ContextPreparationStage::Context, |receiver| {
            *receiver.borrow_and_update()
        });
    let interrupted = *cancel.borrow();
    let preparation = if let Some(by) = interrupted {
        Err(by)
    } else {
        let mut pending = source.prepare_context(step, ordinal, generation);
        let mut deadline = std::pin::pin!(tokio::time::sleep(deadline_bound));
        loop {
            tokio::select! {
                biased;
                changed = cancel.changed(), if *interrupt_available => {
                    if changed.is_err() {
                        *interrupt_available = false;
                    } else if let Some(by) = *cancel.borrow() {
                        break Err(by);
                    }
                }
                changed = async {
                    match &mut progress {
                        Some(receiver) => receiver.changed().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if changed.is_err() { progress = None; }
                    else {
                        if let Some(receiver) = &mut progress {
                            stage = *receiver.borrow_and_update();
                        }
                        deadline.as_mut().reset(tokio::time::Instant::now() + deadline_bound);
                    }
                }
                () = &mut deadline => break Ok(Err(PreparationTimeout::deadline(stage, deadline_bound))),
                prepared = &mut pending => {
                    if let Some(refusal) = prepared.context_preparation_timeout() {
                        // A bounded inner call can finish before our watchdog.
                        // It still requires aborted cleanup before any retry.
                        stage = ContextPreparationStage::Continuity;
                        break Ok(Err(PreparationTimeout { stage, refusal }));
                    }
                    break Ok(Ok(prepared));
                }
            }
        }
    };
    // `pending` was dropped above. No future still borrows the context
    // owner when its bounded close is joined. Cleanup may fail or time out;
    // cancellation still wins, and no provider stream may open.
    let aborted = !matches!(preparation, Ok(Ok(_)));
    let cancelled = aborted || cancel.borrow().is_some();
    let closed = tokio::time::timeout(deadline_bound, source.finish_context(cancelled)).await;
    // Cancellation arriving during cleanup also wins over timeout or refusal.
    let interrupted = (*cancel.borrow()).or(preparation.as_ref().err().copied());
    if let Some(by) = interrupted {
        let detail = match closed {
            Ok(Ok(())) => "interrupted during context preparation".to_owned(),
            Ok(Err(reason)) => {
                format!("interrupted during context preparation; cleanup failed: {reason}")
            }
            Err(_) => {
                "interrupted during context preparation; cleanup deadline exceeded".to_owned()
            }
        };
        return Err(StepFailure::new(
            StepTerminalReason::Interrupted { by },
            detail,
        ));
    }
    match closed {
        // Only confirmed cleanup can expose a retryable preparation timeout.
        Ok(Ok(())) => preparation.map_err(|by| {
            StepFailure::new(
                StepTerminalReason::Interrupted { by },
                "interrupted during context preparation",
            )
        }),
        Ok(Err(detail)) => Ok(Ok(AttemptPreparation::refused(
            AttemptAdmissionRefusal::PlanRefused { detail },
        ))),
        Err(_) => Ok(Ok(AttemptPreparation::refused(
            AttemptAdmissionRefusal::PlanRefused {
                detail: "context resource cleanup deadline exceeded".to_owned(),
            },
        ))),
    }
}
