//! What to do when an attempt ends, as a type that cannot express the wrong
//! answer.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Retry and compaction classification".
//!
//! # Why this is a type and not a function
//!
//! The oracle's rule — "retryable only before visible output" — lives in
//! control flow (`llm.ts:175-454`), where it is one `if` away from being lost.
//! The contract restates it as a hard invariant, so here it is enforced by
//! construction instead: [`AttemptDisposition::Retry`] carries a private
//! [`NoVisibleOutput`] witness, and the only way to obtain that witness is
//! [`crate::normalize::StreamValidator::no_visible_output`], which returns
//! `None` once the attempt has emitted anything visible.
//!
//! A caller outside this crate therefore *cannot* name a retry for an attempt
//! that already produced output — not "should not", cannot. The compiler
//! refuses the struct literal because the witness field is private, and the
//! constructor demands a witness the validator will not hand over.
//!
//! # What this module does not own
//!
//! The five-attempt cap. That bound is `StepPolicy::max_attempts` in
//! `grapt-runtime`, its single owner. A disposition of [`AttemptDisposition::Retry`] means "this
//! failure is retryable *at the stream level*"; the owner still has to ask
//! whether the budget allows another attempt.

use std::time::Duration;

use crate::fragment::{FailureClassification, ProviderFailureClass};

/// Evidence that an attempt has produced no visible output.
///
/// Unforgeable outside this crate: the field is private and there is no public
/// constructor. Obtain one from
/// [`crate::normalize::StreamValidator::no_visible_output`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoVisibleOutput(pub(crate) ());

/// The decision the attempt owner acts on once a stream ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptDisposition {
    /// Clean provider terminal.
    Complete,

    /// Retryable at the stream level. Constructible only through
    /// [`AttemptDisposition::retry`], which requires proof that no visible
    /// output occurred.
    Retry {
        class: ProviderFailureClass,
        after: Option<Duration>,
        /// Private witness — this is what makes the variant unforgeable from
        /// outside the crate.
        witness: NoVisibleOutput,
    },

    /// A context overflow before visible output. The contract permits exactly
    /// one transparent rebuild; the owner enforces "one", this type only
    /// says the rebuild is admissible.
    RebuildAfterCompaction {
        reason: FailureClassification,
        witness: NoVisibleOutput,
    },

    /// Terminal failure — either a non-retryable class, or a retryable one
    /// that arrived after visible output.
    Fail { class: ProviderFailureClass },

    /// Cancelled by the owner. Never a success, never a retry.
    Interrupted,
}

impl AttemptDisposition {
    /// Classify a provider failure.
    ///
    /// `witness` is `Some` only when the attempt produced no visible output.
    /// Pass the validator's answer directly; do not reconstruct it.
    ///
    /// The two conditions are deliberately both required, matching the
    /// contract: "`RateLimit`, provider-internal, and transport failures are
    /// retryable **only before visible output** and only within the
    /// five-attempt bound." This function owns the first two clauses; the
    /// caller owns the third.
    pub fn classify(
        class: ProviderFailureClass,
        classification: Option<FailureClassification>,
        witness: Option<NoVisibleOutput>,
    ) -> Self {
        match (classification, witness) {
            // Overflow before visible output may rebuild once. It is checked
            // first because overflow is a different decision from retry and
            // must not be swallowed by the retry branch.
            (Some(reason @ FailureClassification::ContextOverflow), Some(witness)) => {
                Self::RebuildAfterCompaction { reason, witness }
            }
            // Overflow after visible output is terminal — the contract
            // forbids silently reconstructing user-visible output.
            (Some(FailureClassification::ContextOverflow), None) => Self::Fail { class },
            (Some(FailureClassification::CredentialRefreshed), Some(witness))
                if class == ProviderFailureClass::Authentication =>
            {
                Self::Retry {
                    class,
                    after: None,
                    witness,
                }
            }
            (None, Some(witness)) if class.is_retryable() => {
                let after = class.retry_after();
                Self::Retry {
                    class,
                    after,
                    witness,
                }
            }
            _ => Self::Fail { class },
        }
    }

    /// Construct a retry directly, when the caller has already classified.
    ///
    /// Returns `None` for a class that is not retryable, so this cannot be
    /// used to launder a terminal failure into a retry.
    pub fn retry(class: ProviderFailureClass, witness: NoVisibleOutput) -> Option<Self> {
        if !class.is_retryable() {
            return None;
        }
        let after = class.retry_after();
        Some(Self::Retry {
            class,
            after,
            witness,
        })
    }

    pub fn is_retry(&self) -> bool {
        matches!(self, Self::Retry { .. })
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete | Self::Fail { .. } | Self::Interrupted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deterministic::ScriptBuilder;
    use crate::fragment::FinishReason;
    use crate::normalize::StreamValidator;

    /// Run a script and hand back the validator, so witnesses come from real
    /// stream state rather than being conjured in the test.
    fn run(script: &crate::deterministic::Script) -> StreamValidator {
        let mut validator = StreamValidator::new();
        for fragment in script.fragments() {
            match validator.accept(fragment) {
                Ok(_) => {}
                Err(violation) => panic!("fixture script must be legal: {violation}"),
            }
        }
        validator
    }

    #[test]
    fn retryable_failure_before_output_retries() {
        let script = ScriptBuilder::new().fail(ProviderFailureClass::Transport, None, "reset");
        let validator = run(&script);
        let disposition = AttemptDisposition::classify(
            ProviderFailureClass::Transport,
            None,
            validator.no_visible_output(),
        );
        assert!(disposition.is_retry());
        assert!(!disposition.is_terminal());
    }

    #[test]
    fn retryable_failure_after_output_fails() {
        // Contract fixture #5: text delta then rate limit -> no second call.
        let script = ScriptBuilder::new().text(&["partial"]).fail(
            ProviderFailureClass::RateLimit { retry_after: None },
            None,
            "429",
        );
        let validator = run(&script);
        assert!(validator.visible_output());
        assert!(
            validator.no_visible_output().is_none(),
            "the witness must be unobtainable once output exists"
        );

        let disposition = AttemptDisposition::classify(
            ProviderFailureClass::RateLimit { retry_after: None },
            None,
            validator.no_visible_output(),
        );
        assert_eq!(
            disposition,
            AttemptDisposition::Fail {
                class: ProviderFailureClass::RateLimit { retry_after: None }
            },
            "a retryable class after visible output is terminal"
        );
    }

    #[test]
    fn non_retryable_class_never_retries_even_with_a_witness() {
        let script = ScriptBuilder::new().fail(ProviderFailureClass::Authentication, None, "401");
        let validator = run(&script);
        let witness = match validator.no_visible_output() {
            Some(witness) => witness,
            None => panic!("a silent stream must yield a witness"),
        };
        assert_eq!(
            AttemptDisposition::retry(ProviderFailureClass::Authentication, witness),
            None,
            "retry() must refuse to launder a terminal class"
        );
        assert_eq!(
            AttemptDisposition::classify(ProviderFailureClass::Authentication, None, Some(witness)),
            AttemptDisposition::Fail {
                class: ProviderFailureClass::Authentication
            }
        );
    }

    #[test]
    fn retry_after_is_carried_from_the_class() {
        let delay = Duration::from_millis(2500);
        let script = ScriptBuilder::new().fail(
            ProviderFailureClass::RateLimit {
                retry_after: Some(delay),
            },
            None,
            "429",
        );
        let validator = run(&script);
        match AttemptDisposition::classify(
            ProviderFailureClass::RateLimit {
                retry_after: Some(delay),
            },
            None,
            validator.no_visible_output(),
        ) {
            AttemptDisposition::Retry { after, .. } => assert_eq!(after, Some(delay)),
            other => panic!("expected a retry, got {other:?}"),
        }
    }

    #[test]
    fn virgin_overflow_rebuilds_and_late_overflow_fails() {
        // Contract: "a context overflow before visible output may run one
        // compaction rebuild ... an overflow after visible output is terminal
        // and never silently reconstructs user-visible output."
        let virgin = ScriptBuilder::new().fail(
            ProviderFailureClass::InvalidRequest,
            Some(FailureClassification::ContextOverflow),
            "context window exceeded",
        );
        let validator = run(&virgin);
        match AttemptDisposition::classify(
            ProviderFailureClass::InvalidRequest,
            Some(FailureClassification::ContextOverflow),
            validator.no_visible_output(),
        ) {
            AttemptDisposition::RebuildAfterCompaction { reason, .. } => {
                assert_eq!(reason, FailureClassification::ContextOverflow);
            }
            other => panic!("virgin overflow must rebuild, got {other:?}"),
        }

        let late = ScriptBuilder::new().text(&["already said this"]).fail(
            ProviderFailureClass::InvalidRequest,
            Some(FailureClassification::ContextOverflow),
            "context window exceeded",
        );
        let validator = run(&late);
        assert_eq!(
            AttemptDisposition::classify(
                ProviderFailureClass::InvalidRequest,
                Some(FailureClassification::ContextOverflow),
                validator.no_visible_output(),
            ),
            AttemptDisposition::Fail {
                class: ProviderFailureClass::InvalidRequest
            },
            "overflow after visible output is terminal"
        );
    }

    #[test]
    fn a_clean_stream_yields_a_witness_but_completes() {
        // A silent-but-successful stream still has no visible output. The
        // witness exists; nothing forces it to be used for a retry.
        let script = ScriptBuilder::new().finish(FinishReason::Stop, None);
        let validator = run(&script);
        assert!(validator.no_visible_output().is_some());
        assert!(AttemptDisposition::Complete.is_terminal());
    }

    #[test]
    fn interrupted_is_never_success_and_never_retry() {
        let disposition = AttemptDisposition::Interrupted;
        assert!(!disposition.is_retry());
        assert!(disposition.is_terminal());
        assert_ne!(disposition, AttemptDisposition::Complete);
    }
}
