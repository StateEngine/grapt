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
        coordinate: (StepNumber, u32),
        refusal: AttemptAdmissionRefusal,
    ) -> Result<(), StepResult> {
        let (step, ordinal) = coordinate;
        if matches!(
            refusal,
            AttemptAdmissionRefusal::CapacityBusy | AttemptAdmissionRefusal::Saturated { .. }
        ) {
            let since = self.since.get_or_insert_with(tokio::time::Instant::now);
            self.retries = self.retries.saturating_add(1);
            let after = admission_delay(
                policy,
                step,
                ordinal,
                self.retries,
                source.jitter_coordinate(),
            );
            let notice = RetryNotice {
                attempt: ordinal,
                after,
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

fn admission_delay(
    policy: &StepPolicy,
    step: StepNumber,
    ordinal: u32,
    retries: u32,
    coordinate: Option<&str>,
) -> Duration {
    let base = policy.backoff_for(retries).min(policy.max_retry_delay);
    let source_salt = coordinate.map_or(0, backoff::salt_from_str);
    let salt = source_salt ^ u64::from(ordinal).wrapping_shl(32) ^ u64::from(retries);
    backoff::jittered(base, step, retries, salt, policy.max_retry_delay)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummySource {
        session: String,
    }

    impl AttemptSource for DummySource {
        type Reader = crate::provider_source::ProviderStreamReader;

        fn jitter_coordinate(&self) -> Option<&str> {
            Some(self.session.as_str())
        }

        fn prepare(
            &mut self,
            _step: StepNumber,
            _ordinal: u32,
            _generation: HistoryGeneration,
        ) -> AttemptPreparation<Self::Reader> {
            AttemptPreparation::refused(AttemptAdmissionRefusal::CapacityBusy)
        }
    }

    #[test]
    fn admission_jitter_depends_on_stable_coordinates_not_address() -> Result<(), String> {
        let mut policy = StepPolicy::ruled_default().map_err(|e| e.to_string())?;
        policy.base_backoff = Duration::from_secs(1);
        policy.max_retry_delay = Duration::from_secs(60);
        let step = StepNumber::new(1).ok_or("step 1")?;
        let first = Box::new(DummySource {
            session: "ses_same_coordinate".to_owned(),
        });
        let second = Box::new(DummySource {
            session: "ses_same_coordinate".to_owned(),
        });
        assert_ne!(
            std::ptr::from_ref(first.as_ref()),
            std::ptr::from_ref(second.as_ref())
        );

        let d1 = admission_delay(&policy, step, 1, 1, first.jitter_coordinate());
        let d2 = admission_delay(&policy, step, 1, 1, second.jitter_coordinate());
        assert_eq!(d1, d2);

        let other = Box::new(DummySource {
            session: "ses_other_coordinate".to_owned(),
        });
        let d_other = admission_delay(&policy, step, 1, 1, other.jitter_coordinate());
        assert_ne!(d1, d_other);
        assert!(d1 >= Duration::from_secs(1) && d1 <= Duration::from_millis(1250));
        assert!(d_other >= Duration::from_secs(1) && d_other <= Duration::from_millis(1250));
        Ok(())
    }
}
