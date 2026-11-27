//! Closed terminal vocabularies.
//!
//! Law (`CORRECT`): there is no `done: bool`. Domain-terminal,
//! transport-closed, cleanup-complete, delivery-complete and
//! ambiguous-external-effect are five distinct facts, and a completed process
//! may still be undelivered, undrained or externally ambiguous.
//!
//! Amendment §6.2: bare strings such as `provider-failure`, `aborted` or
//! `process-loss` never decide control flow. Every terminal reason below is a
//! closed, data-carrying enum whose payload is required at construction.

use crate::authority::{AuthorityPrincipal, PolicyVersion};
use crate::completion::ProviderCompletionDisposition;
use crate::operation::AmbiguityEvidence;

/// Whether an external effect happened, did not happen, or cannot be known
/// without inspection.
///
/// This is the fact that makes automatic replay illegal: an ambiguous effect
/// stays ambiguous until exact inspection settles it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalEffectState {
    /// No external effect was initiated.
    NoneInitiated,
    /// The external effect completed and is exactly known.
    Settled,
    /// An effect may or may not have occurred. Never auto-replay this.
    Ambiguous { evidence: AmbiguityEvidence },
}

impl ExternalEffectState {
    /// Whether it is safe to retry the work that produced this state.
    pub fn safe_to_replay(&self) -> bool {
        matches!(self, ExternalEffectState::NoneInitiated)
    }
}

/// Who or what stopped in-flight work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterruptionSource {
    /// An explicit user interrupt.
    User,
    /// A managed shutdown of the owning runtime.
    ManagedShutdown,
}

/// Closed provider failure classification.
///
/// Retry *policy* is provider-specific and lives in the provider crate; the
/// classification vocabulary is foundational because terminal facts name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderFailureClass {
    /// Rate limited. Retryable before visible output.
    RateLimit,
    /// The provider reported an internal error. Retryable before output.
    ProviderInternal,
    /// Transport-level failure. Retryable before output.
    Transport,
    /// The stream made no progress within its typed budget.
    NoProgressTimeout,
    /// Credentials rejected. Terminal.
    Authentication,
    /// Quota exhausted. Terminal.
    Quota,
    /// Refused by provider content policy. Terminal.
    ContentPolicy,
    /// The request was not acceptable to the provider. Terminal.
    InvalidRequest,
    /// The provider produced output the adapter cannot decode. Terminal.
    InvalidProviderOutput,
    /// No adapter claims this provider. Terminal.
    UnknownProvider,
    /// No route resolved for the target. Terminal.
    NoRoute,
    /// The local runtime refused the attempt with zero provider bytes: an
    /// integrity, I/O or capacity precondition failed before the provider
    /// could see anything. A minted identity or staged reader may exist;
    /// provider bytes never did. Terminal.
    LocalRefusal,
}

impl ProviderFailureClass {
    /// Whether this class is eligible for retry at all.
    ///
    /// Eligibility is necessary but not sufficient: the runtime additionally
    /// requires that no visible output exists and that the attempt bound has
    /// not been reached.
    pub fn is_retry_eligible(self) -> bool {
        matches!(
            self,
            ProviderFailureClass::RateLimit
                | ProviderFailureClass::ProviderInternal
                | ProviderFailureClass::Transport
                | ProviderFailureClass::NoProgressTimeout
        )
    }
}

/// Closed context/compaction failure classification.
///
/// Law §6.5: recoverable context exhaustion is not completion, blockage or a
/// provider retry. An exhausted compaction budget is its own truthful reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextFailure {
    /// Required external continuity could not be established for the bound principal.
    ContinuityUnavailable,
    /// The prepared request exceeded the model's context.
    Overflow,
    /// A compaction attempt failed.
    CompactionFailed,
    /// Compaction ran but reclaimed nothing; the bound stopped the loop.
    CompactionNoProgress,
    /// Every call since the last compaction repeated what it summarized and
    /// the request no longer fits: compacting again would only loop.
    CompactionRepeated,
    /// A provider-bound summary cannot be redeemed against this target.
    CompactionIncompatibleSummary,
}

/// Why one physical provider attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptTerminalReason {
    /// The provider closed the stream cleanly with its disposition.
    ProviderEnded {
        completion: ProviderCompletionDisposition,
    },
    /// The provider or its transport failed.
    ProviderFailure { class: ProviderFailureClass },
    /// Context preparation or compaction failed.
    ContextFailure { failure: ContextFailure },
    /// Cancelled by an interrupt or managed shutdown.
    Interrupted { by: InterruptionSource },
    /// Refused before any provider bytes were sent.
    AuthorityRefused { principal: AuthorityPrincipal },
    /// A durable append failed, so nothing may be published.
    DurabilityFailure,
    /// The owning process died. The external effect state decides whether any
    /// continuation is legal.
    ProcessLoss { effect: ExternalEffectState },
}

/// Why one logical Step ended.
///
/// A Step may span several attempts, so its reasons include the policy and
/// budget outcomes an individual attempt cannot observe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepTerminalReason {
    /// Ended normally, carrying the provider's completion disposition.
    ///
    /// Law: this ends the owned Step chain only. It never settles an active
    /// durable objective or verifies a campaign node.
    Ended {
        completion: ProviderCompletionDisposition,
    },
    /// The model asserted completion. Distinct from `Ended`: an assertion is
    /// not evidence.
    ModelAssertedComplete,
    /// A versioned policy limit stopped the Step.
    PolicyLimit { policy: PolicyVersion },
    /// A declared budget was exhausted.
    BudgetExhausted { policy: PolicyVersion },
    /// Cancelled by an interrupt or managed shutdown.
    Interrupted { by: InterruptionSource },
    /// The provider failed terminally for this Step.
    ProviderFailure { class: ProviderFailureClass },
    /// Context preparation or compaction failed terminally.
    ContextFailure { failure: ContextFailure },
    /// Refused by the authority boundary.
    AuthorityRefused { principal: AuthorityPrincipal },
    /// A durable append failed.
    DurabilityFailure,
    /// The owning process died mid-Step.
    ProcessLoss { effect: ExternalEffectState },
    /// A failure reason this build's vocabulary does not share: the durable
    /// payload was written by a later build and named a kind or word this
    /// reader does not know.
    ///
    /// Read-only: decoders are the only producers. This build's writers
    /// cannot construct it (the failure fact's checked constructor refuses
    /// it) and no encoder may write it back. `tag` keeps the raw unknown
    /// word, byte-capped, for diagnosis; it is untrusted text and must be
    /// sanitized before any display.
    Unknown { tag: String },
}

impl StepTerminalReason {
    /// Whether the Step reached its intended end rather than failing.
    ///
    /// Deliberately false for [`StepTerminalReason::ModelAssertedComplete`]:
    /// an assertion is recorded, not believed.
    pub fn is_success(&self) -> bool {
        matches!(self, StepTerminalReason::Ended { .. })
    }
}

/// The domain-terminal fact of a durable operation.
///
/// Reaching this does **not** imply delivery or cleanup; those are separate
/// vocabularies below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationTerminal {
    /// The operation's own work finished successfully.
    Completed,
    /// The operation failed for a typed reason.
    Failed { reason: OperationFailure },
    /// Cancelled before completion.
    Cancelled { by: InterruptionSource },
    /// The owning runtime died. Never resumed while the effect is ambiguous.
    ProcessLoss { effect: ExternalEffectState },
}

/// Closed operation failure classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationFailure {
    /// The authority boundary refused it.
    AuthorityRefused { principal: AuthorityPrincipal },
    /// A declared budget was exhausted.
    BudgetExhausted { policy: PolicyVersion },
    /// The underlying effect reported failure.
    EffectFailed,
    /// A durable append failed.
    DurabilityFailure,
}

/// Delivery settlement, independent of the operation's domain terminal.
///
/// Law: `DomainTerminal` does not imply delivered. Failed or quarantined
/// delivery remains owed, so notification debt cannot contradict a stopped
/// operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryTerminal {
    /// The recipient's durable inbox admitted the delivery.
    Delivered,
    /// The recipient additionally acknowledged it.
    Acknowledged,
    /// Delivery failed and is still owed.
    Failed { reason: DeliveryFailure },
    /// Delivery is withheld pending inspection and is still owed.
    Quarantined { reason: DeliveryFailure },
}

impl DeliveryTerminal {
    /// Whether the notification debt is discharged.
    pub fn is_settled(&self) -> bool {
        matches!(
            self,
            DeliveryTerminal::Delivered | DeliveryTerminal::Acknowledged
        )
    }
}

/// Why a delivery did not settle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryFailure {
    /// The recipient route no longer exists.
    RecipientGone,
    /// The claim was lost before the recipient recorded delivery.
    ClaimLost,
    /// The recipient refused the payload.
    Refused,
    /// A durable append failed.
    DurabilityFailure,
}

/// Cleanup settlement, independent of both domain terminal and delivery.
///
/// Law: shell adds `PipeDrained` and `CleanupComplete`; neither is inferred
/// from child exit. A process that exited while a descendant still holds an
/// inherited pipe is exited, not drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupTerminal {
    /// Every inherited pipe reached EOF.
    PipeDrained,
    /// Every owned resource was released after the drain.
    CleanupComplete,
    /// The bounded cleanup budget expired. `drained` records how far it got;
    /// it never upgrades to `CleanupComplete`.
    TimedOut { drained: bool },
}

impl CleanupTerminal {
    /// Whether cleanup is fully settled.
    pub fn is_complete(self) -> bool {
        matches!(self, CleanupTerminal::CleanupComplete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_completed_operation_may_still_be_undelivered_and_undrained() {
        let domain = OperationTerminal::Completed;
        let delivery = DeliveryTerminal::Failed {
            reason: DeliveryFailure::ClaimLost,
        };
        let cleanup = CleanupTerminal::TimedOut { drained: false };

        assert!(matches!(domain, OperationTerminal::Completed));
        assert!(
            !delivery.is_settled(),
            "a lost claim leaves the notification debt owed"
        );
        assert!(
            !cleanup.is_complete(),
            "a timed-out cleanup never upgrades to complete"
        );
    }

    #[test]
    fn quarantined_delivery_is_still_owed() {
        let quarantined = DeliveryTerminal::Quarantined {
            reason: DeliveryFailure::Refused,
        };
        assert!(!quarantined.is_settled());
        assert!(DeliveryTerminal::Delivered.is_settled());
        assert!(DeliveryTerminal::Acknowledged.is_settled());
    }

    #[test]
    fn ambiguous_effect_is_never_safe_to_replay() -> Result<(), Box<dyn std::error::Error>> {
        let ambiguous = ExternalEffectState::Ambiguous {
            evidence: AmbiguityEvidence::new("request_sent, no terminal")
                .ok_or("ambiguity evidence")?,
        };
        assert!(!ambiguous.safe_to_replay());
        assert!(!ExternalEffectState::Settled.safe_to_replay());
        assert!(ExternalEffectState::NoneInitiated.safe_to_replay());
        Ok(())
    }

    #[test]
    fn a_model_assertion_is_not_a_successful_step() {
        assert!(!StepTerminalReason::ModelAssertedComplete.is_success());
        assert!(StepTerminalReason::Ended {
            completion: ProviderCompletionDisposition::AssertedComplete,
        }
        .is_success());
    }

    #[test]
    fn truncated_is_not_an_ordinary_ending() {
        // Law: provider `length` maps to `Truncated` and enters recovery or a
        // typed failure — it must never fall through to idle. `Truncated`
        // remains distinguishable inside a successful-shaped Step end.
        let truncated = StepTerminalReason::Ended {
            completion: ProviderCompletionDisposition::Truncated,
        };
        match truncated {
            StepTerminalReason::Ended { completion } => {
                assert_ne!(completion, ProviderCompletionDisposition::AssertedComplete);
                assert!(!completion.asserted_complete());
            }
            _ => unreachable!("constructed as Ended"),
        }
    }

    #[test]
    fn terminal_provider_classes_are_not_retry_eligible() {
        for class in [
            ProviderFailureClass::Authentication,
            ProviderFailureClass::Quota,
            ProviderFailureClass::ContentPolicy,
            ProviderFailureClass::InvalidRequest,
            ProviderFailureClass::InvalidProviderOutput,
            ProviderFailureClass::UnknownProvider,
            ProviderFailureClass::NoRoute,
            ProviderFailureClass::LocalRefusal,
        ] {
            assert!(!class.is_retry_eligible(), "{class:?} must be terminal");
        }
        for class in [
            ProviderFailureClass::RateLimit,
            ProviderFailureClass::ProviderInternal,
            ProviderFailureClass::Transport,
            ProviderFailureClass::NoProgressTimeout,
        ] {
            assert!(class.is_retry_eligible(), "{class:?} must be eligible");
        }
    }

    #[test]
    fn process_loss_carries_its_effect_state() -> Result<(), Box<dyn std::error::Error>> {
        let lost = AttemptTerminalReason::ProcessLoss {
            effect: ExternalEffectState::Ambiguous {
                evidence: AmbiguityEvidence::new("request_sent").ok_or("ambiguity evidence")?,
            },
        };
        match lost {
            AttemptTerminalReason::ProcessLoss { effect } => {
                assert!(!effect.safe_to_replay());
            }
            _ => unreachable!("constructed as ProcessLoss"),
        }
        Ok(())
    }

    #[test]
    fn ambiguity_must_say_what_was_observed() {
        assert!(
            AmbiguityEvidence::new("").is_none(),
            "blank evidence is not evidence"
        );
        assert!(AmbiguityEvidence::new("request_sent").is_some());
    }
}
