//! The vocabulary a RESUMED Step is admitted through.
//!
//! Split out of `durable_owner` rather than left beside the owner because it is
//! a self-contained answer to one question — may this Step continue, and under
//! which attempt — and because the owner module had reached the ceiling the
//! workspace's own god-file gate enforces. The types are re-exported from
//! `durable_owner`, so no caller learns that this file exists.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Attempt lifecycle, tools, and durable order" ("a retry is not a new
//! Step"), and the no-provider-resurrection rule: an attempt that has already
//! delivered visible output is never silently continued.

use grapt_types::{ContentOrdinal, MessageId, PhysicalAttemptId};

/// An assistant message this Step already owns, and where its content
/// continues.
///
/// The two coordinates travel TOGETHER and that is the whole point. An earlier
/// revision accepted a bare `ContentOrdinal`, which let a caller hand a fresh
/// owner a non-zero starting ordinal: the owner then minted a NEW message whose
/// first item claimed ordinal one, and no dense sequence from
/// [`ContentOrdinal::FIRST`] could ever be built from it. The read fold refused
/// it — correctly — but the defect was written by then. Pairing the identity
/// with its ordinal makes the broken combination unnameable: a resumed ordinal
/// exists only for a message that already exists.
///
/// Derive it from DURABLE truth
/// (`grapt_session::MessageHistory::next_ordinal`), never from a counter a
/// restart would reset.
///
/// # Why identity and ordinal are not enough
///
/// They say WHERE to continue. They say nothing about WHETHER continuing is
/// admitted, and an earlier revision asked only the first question: a caller
/// that could name a message and an ordinal could reopen any Step, under any
/// attempt, after any amount of visible output — which is automatic provider
/// resurrection performed by the owner itself. The two coordinates below are
/// the second question, and they travel with the first for the same reason the
/// ordinal travels with the identity: the broken combination has to be
/// unnameable, not merely discouraged.
#[derive(Debug, Clone)]
pub struct ResumedMessage {
    message: MessageId,
    next_ordinal: ContentOrdinal,
    reconciliation: ProcessLossReconciliation,
    authority: ContinuationAuthority,
}

/// What durable truth says about the attempt that was streaming when the
/// process was lost.
///
/// There is deliberately no "probably fine" member. An attempt whose fate the
/// log cannot state is [`Self::Unterminated`], and that is a refusal rather
/// than a default, because the failure this distinguishes — continuing a Step
/// whose earlier attempt may still be live somewhere — is exactly the one a
/// hopeful default produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorAttempt {
    /// Durably reconciled as failed BEFORE any of its output was visible.
    /// The only disposition a continuation may be built on.
    FailedBeforeVisibleOutput,
    /// Durably reconciled as failed AFTER its output reached the user.
    /// Continuing here is provider resurrection: the user has already seen a
    /// partial answer and a second attempt would silently extend it.
    FailedAfterVisibleOutput,
    /// Durable truth carries no terminal for it at all.
    Unterminated,
}

/// The typed reconciliation of one process loss.
///
/// Constructed by the caller that actually READ durable truth after the
/// restart. The owner cannot derive any of this — it is brand new and has seen
/// nothing — so requiring it as an argument is what makes "the runtime
/// reconciled the loss" a fact the owner checked rather than a story it
/// assumed.
#[derive(Debug, Clone)]
pub struct ProcessLossReconciliation {
    lost_attempt: PhysicalAttemptId,
    prior: PriorAttempt,
    step_settled: bool,
}

impl ProcessLossReconciliation {
    /// `step_settled` is whether the Step already wrote its ONE durable
    /// terminal. A settled Step has no continuation: it is finished, and
    /// reopening it would resurrect a Step rather than an attempt.
    pub fn new(lost_attempt: PhysicalAttemptId, prior: PriorAttempt, step_settled: bool) -> Self {
        Self {
            lost_attempt,
            prior,
            step_settled,
        }
    }

    pub fn lost_attempt(&self) -> &PhysicalAttemptId {
        &self.lost_attempt
    }

    pub fn prior(&self) -> PriorAttempt {
        self.prior
    }

    pub fn step_settled(&self) -> bool {
        self.step_settled
    }
}

/// Explicit authority to continue a Step's message with a NAMED attempt.
///
/// Both attempts are named, and that is the point. An authority that named only
/// "yes, continue" would admit any attempt the caller later happened to open,
/// which is the silent attempt swap: the message would grow under an identity
/// nobody authorized and the durable record would show one message spanning two
/// physical attempts with no fact saying so.
#[derive(Debug, Clone)]
pub struct ContinuationAuthority {
    lost_attempt: PhysicalAttemptId,
    continuation_attempt: PhysicalAttemptId,
}

impl ContinuationAuthority {
    pub fn grant(lost_attempt: PhysicalAttemptId, continuation_attempt: PhysicalAttemptId) -> Self {
        Self {
            lost_attempt,
            continuation_attempt,
        }
    }

    pub fn continuation_attempt(&self) -> &PhysicalAttemptId {
        &self.continuation_attempt
    }
}

/// Why a resumption was refused.
///
/// Typed per reason rather than one string: each member names a DIFFERENT
/// broken caller, and a consumer that could only see "refused" would retry the
/// three unrecoverable ones as if they were the recoverable one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResumeRefused {
    #[error("step already wrote its one durable terminal; it has no continuation")]
    StepAlreadyTerminal,
    #[error(
        "the lost attempt {attempt} carries no durable terminal; an unterminated \
         attempt cannot be continued past"
    )]
    AmbiguousPriorAttempt { attempt: String },
    #[error(
        "the lost attempt {attempt} had already delivered visible output; \
         continuing it would resurrect the provider stream"
    )]
    VisibleOutputAlreadyDelivered { attempt: String },
    #[error(
        "the continuation authority names lost attempt {authorized}, but the \
         reconciliation names {reconciled}"
    )]
    AttemptSwap {
        authorized: String,
        reconciled: String,
    },
    #[error(
        "the continuation names the lost attempt {attempt} as its own \
         continuation; a retry is a NEW physical attempt"
    )]
    ContinuationIsTheLostAttempt { attempt: String },
}

impl ResumedMessage {
    /// The only constructor, and it REFUSES every combination the contract
    /// forbids. See the type's own documentation for why the refusal lives
    /// here and not at the call site.
    pub fn new(
        message: MessageId,
        next_ordinal: ContentOrdinal,
        reconciliation: ProcessLossReconciliation,
        authority: ContinuationAuthority,
    ) -> Result<Self, ResumeRefused> {
        if reconciliation.step_settled {
            return Err(ResumeRefused::StepAlreadyTerminal);
        }
        match reconciliation.prior {
            PriorAttempt::FailedBeforeVisibleOutput => {}
            PriorAttempt::Unterminated => {
                return Err(ResumeRefused::AmbiguousPriorAttempt {
                    attempt: reconciliation.lost_attempt.as_str().to_owned(),
                })
            }
            PriorAttempt::FailedAfterVisibleOutput => {
                return Err(ResumeRefused::VisibleOutputAlreadyDelivered {
                    attempt: reconciliation.lost_attempt.as_str().to_owned(),
                })
            }
        }
        if authority.lost_attempt != reconciliation.lost_attempt {
            return Err(ResumeRefused::AttemptSwap {
                authorized: authority.lost_attempt.as_str().to_owned(),
                reconciled: reconciliation.lost_attempt.as_str().to_owned(),
            });
        }
        if authority.continuation_attempt == reconciliation.lost_attempt {
            return Err(ResumeRefused::ContinuationIsTheLostAttempt {
                attempt: reconciliation.lost_attempt.as_str().to_owned(),
            });
        }
        Ok(Self {
            message,
            next_ordinal,
            reconciliation,
            authority,
        })
    }

    pub fn message(&self) -> &MessageId {
        &self.message
    }

    pub fn next_ordinal(&self) -> ContentOrdinal {
        self.next_ordinal
    }

    pub fn reconciliation(&self) -> &ProcessLossReconciliation {
        &self.reconciliation
    }

    /// The ONE attempt this resumption admits.
    pub fn continuation_attempt(&self) -> &PhysicalAttemptId {
        &self.authority.continuation_attempt
    }
}
