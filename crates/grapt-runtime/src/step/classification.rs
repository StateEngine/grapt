use grapt_provider::{FailureClassification, ProviderFailureClass};
use grapt_types::{
    AttemptTerminalReason, ContextFailure, ProviderFailureClass as TerminalClass,
    StepTerminalReason,
};

use crate::supervisor::{AttemptReport, StepOutcome};

use super::{AttemptDecision, StepPolicy};

/// The whole policy decision, as one pure function over the attempt's result.
///
/// `attempts_used` is the 1-based ordinal of the attempt just finished, so the
/// bound covers retries and rebuilds together.
pub(super) fn classify(
    report: &AttemptReport,
    policy: &StepPolicy,
    timed_out: bool,
    attempts_used: u32,
    rebuilds: u32,
) -> AttemptDecision {
    if matches!(report.outcome, StepOutcome::OwnerRejected { .. })
        || matches!(report.outcome, StepOutcome::Ended { .. })
    {
        return AttemptDecision::Settled;
    }

    let exhausted = attempts_used >= policy.max_attempts;

    // A no-progress stall is a typed timeout, transport-class for retry
    // purposes (a pre-output no-progress timeout IS retryable).
    if timed_out {
        // A stall that produced output is a provider failure of the stall's own
        // class, NOT an interruption: no one interrupted anything.
        if report.diagnostics.visible_output {
            return AttemptDecision::Stop {
                reason: StepTerminalReason::ProviderFailure {
                    class: TerminalClass::NoProgressTimeout,
                },
            };
        }
        // A stall that used up the attempt bound is the BOUND stopping the
        // Step, so the reason names the policy that set it.
        if exhausted {
            return AttemptDecision::Stop {
                reason: StepTerminalReason::PolicyLimit {
                    policy: policy.version.clone(),
                },
            };
        }
        return AttemptDecision::Retry {
            after: policy.delay_for(attempts_used, &ProviderFailureClass::Transport),
        };
    }

    if report.diagnostics.cancelled {
        // The attempt already named WHY it was cancelled, and the supervisor is
        // the only place entitled to say so. `diagnostics.cancelled` is a bare
        // bool, so deriving a reason from it here would have to invent one —
        // which is exactly how the watchdog used to come out labelled
        // `aborted`. Carry the attempt's reason up instead.
        return match &report.outcome {
            StepOutcome::Failed { failure } => AttemptDecision::Stop {
                reason: carried_up(failure.reason()),
            },
            // Ended and OwnerRejected already returned Settled above; this arm
            // is unreachable and is represented rather than panicked on.
            _ => AttemptDecision::Settled,
        };
    }

    let Some(detail) = report.provider_failure.as_ref() else {
        return AttemptDecision::Settled;
    };

    if detail.classification == Some(FailureClassification::ContextOverflow) {
        if report.diagnostics.visible_output {
            return AttemptDecision::Stop {
                reason: StepTerminalReason::ContextFailure {
                    failure: ContextFailure::Overflow,
                },
            };
        }
        if rebuilds >= policy.max_overflow_rebuilds {
            return AttemptDecision::Stop {
                reason: StepTerminalReason::ContextFailure {
                    failure: ContextFailure::Overflow,
                },
            };
        }
        if exhausted {
            return AttemptDecision::Stop {
                reason: StepTerminalReason::PolicyLimit {
                    policy: policy.version.clone(),
                },
            };
        }
        return AttemptDecision::Rebuild;
    }

    let credential_recovered = detail.class == ProviderFailureClass::Authentication
        && detail.classification == Some(FailureClassification::CredentialRefreshed);
    if !detail.class.is_retryable() && !credential_recovered {
        return AttemptDecision::Settled;
    }
    // A retryable failure that already produced output stops the Step, and the
    // Step keeps the provider's OWN class. The string carrier discarded it:
    // every such stop read `retryable-failure-after-output`, so a rate limit
    // and a transport drop were indistinguishable downstream.
    if report.diagnostics.visible_output {
        return AttemptDecision::Stop {
            reason: StepTerminalReason::ProviderFailure {
                class: detail.class.to_terminal(),
            },
        };
    }
    if exhausted {
        return AttemptDecision::Stop {
            reason: StepTerminalReason::PolicyLimit {
                policy: policy.version.clone(),
            },
        };
    }
    AttemptDecision::Retry {
        after: policy.delay_for(attempts_used, &detail.class),
    }
}

/// Carry an attempt's terminal reason up to the Step that adopted it.
///
/// Total and lossless: every payload survives. The two vocabularies stay
/// distinct TYPES so that a Step reason cannot be written where an attempt
/// reason belongs, but where a Step genuinely adopts its last attempt's
/// outcome, nothing is flattened into a coarser bucket on the way up.
///
/// `AttemptTerminalReason::ProviderEnded` maps to `StepTerminalReason::Ended`
/// rather than to `ModelAssertedComplete`: the two differ in whether the model
/// merely CLAIMED completion, and that distinction is carried by the
/// disposition inside `completion`, not by the arm.
pub(super) fn carried_up(reason: &AttemptTerminalReason) -> StepTerminalReason {
    match reason {
        AttemptTerminalReason::ProviderEnded { completion } => StepTerminalReason::Ended {
            completion: *completion,
        },
        AttemptTerminalReason::ProviderFailure { class } => {
            StepTerminalReason::ProviderFailure { class: *class }
        }
        AttemptTerminalReason::ContextFailure { failure } => {
            StepTerminalReason::ContextFailure { failure: *failure }
        }
        AttemptTerminalReason::Interrupted { by } => StepTerminalReason::Interrupted { by: *by },
        AttemptTerminalReason::AuthorityRefused { principal } => {
            StepTerminalReason::AuthorityRefused {
                principal: principal.clone(),
            }
        }
        AttemptTerminalReason::DurabilityFailure => StepTerminalReason::DurabilityFailure,
        AttemptTerminalReason::ProcessLoss { effect } => StepTerminalReason::ProcessLoss {
            effect: effect.clone(),
        },
    }
}

/// Diagnose only provider-only retry exhaustion, from this Step's own receipts.
pub(super) fn with_retry_diagnosis(
    mut result: super::StepResult,
    receipts: &[super::AttemptReceipt],
) -> super::StepResult {
    use grapt_types::failure_diagnostic::{RetryExhaustion, RetryFailureClass};
    let super::StepResult::Failed { failure } = &mut result else {
        return result;
    };
    if !matches!(failure.reason(), StepTerminalReason::PolicyLimit { .. }) || receipts.len() > 5 {
        return result;
    }
    let mut classes = Vec::with_capacity(receipts.len());
    for receipt in receipts {
        if matches!(receipt.decision, AttemptDecision::Rebuild) {
            return result;
        }
        let StepOutcome::Failed { failure } = &receipt.outcome else {
            return result;
        };
        let class = match failure.reason() {
            AttemptTerminalReason::ProviderFailure {
                class: TerminalClass::RateLimit,
            } => RetryFailureClass::RateLimited,
            AttemptTerminalReason::ProviderFailure {
                class: TerminalClass::ProviderInternal,
            } => RetryFailureClass::ServerError,
            AttemptTerminalReason::ProviderFailure {
                class: TerminalClass::Transport | TerminalClass::NoProgressTimeout,
            } => RetryFailureClass::Transport,
            _ => return result,
        };
        classes.push(class);
    }
    if let Some(diagnostic) = RetryExhaustion::new(&classes) {
        failure.set_retry_diagnostic(diagnostic);
    }
    result
}
