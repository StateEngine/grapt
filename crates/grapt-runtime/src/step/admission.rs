//! Recover transient local admission contention without spending an attempt.
use super::*;

#[derive(Default)]
pub(super) struct CapacityWait {
    since: Option<tokio::time::Instant>,
    retries: u32,
}

impl CapacityWait {
    pub(super) fn admitted(&mut self) {
        *self = Self::default();
    }

    pub(super) async fn refused<S: AttemptSource>(
        &mut self,
        source: &mut S,
        cancel: &mut watch::Receiver<Option<InterruptionSource>>,
        interrupt_available: &mut bool,
        policy: &StepPolicy,
        ordinal: u32,
        refusal: AttemptAdmissionRefusal,
    ) -> Result<(), StepResult> {
        if refusal == AttemptAdmissionRefusal::CapacityBusy {
            let since = self.since.get_or_insert_with(tokio::time::Instant::now);
            self.retries = self.retries.saturating_add(1);
            let notice = RetryNotice {
                attempt: ordinal,
                after: policy.backoff_for(self.retries).min(policy.max_retry_delay),
                class: None,
                preparation_stage: None,
                capacity_wait: Some(since.elapsed()),
            };
            return backoff::wait(source, cancel, interrupt_available, notice)
                .await
                .map_err(|failure| StepResult::Failed { failure });
        }
        let reason = match &refusal {
            AttemptAdmissionRefusal::ContextUnavailable { failure, .. } => {
                StepTerminalReason::ContextFailure { failure: *failure }
            }
            AttemptAdmissionRefusal::ContextPreparationTimedOut { .. } => {
                StepTerminalReason::PolicyLimit {
                    policy: policy.version.clone(),
                }
            }
            // Genuine local refusals sent no provider bytes and remain terminal.
            _ => StepTerminalReason::ProviderFailure {
                class: TerminalClass::LocalRefusal,
            },
        };
        let failure = StepFailure::new(reason, refusal.to_string());
        let failure = match refusal.admission_diagnostic() {
            Some(diagnostic) => failure.with_admission_refusal(diagnostic),
            None => failure,
        };
        Err(StepResult::AdmissionRefused { failure, refusal })
    }
}
