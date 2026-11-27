//! Cause-specific automatic compaction suppression; the owning Step still runs every call.
//!
//! Copyright 2023-2026 SpaceXAI. Modified by Grapt: typed causes, bounded passive
//! session state, existing target identities and durable recovery observations.

use super::*;
use grapt_provider::{FailureClassification, ProviderFailureClass};
use grapt_runtime::supervisor::ProviderFailureDetail;
use grapt_types::{AccountId, CredentialVersion, ModelRef, ResolvedModelTarget};
use std::collections::BTreeMap;

const MAX_SUPPRESSED_SESSIONS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::dogfood) enum Cause {
    Context,
    Authentication,
    Quota,
    UsageLimit {
        reset: Option<grapt_types::provider_limit::ProviderLimitReset>,
    },
    Transient,
    Other,
    NoProgress,
    Capacity,
}

impl Cause {
    pub(super) fn of_compaction(
        result: &Result<(), grapt_runtime::CompactionFailure>,
        transport: Option<Self>,
    ) -> Self {
        use grapt_runtime::CompactionFailure;
        match result {
            Err(CompactionFailure::NoProgress { .. } | CompactionFailure::Repeated { .. }) => {
                Self::NoProgress
            }
            _ => transport.unwrap_or(Self::Other),
        }
    }

    pub(super) fn of(failure: Option<&ProviderFailureDetail>) -> Self {
        let Some(failure) = failure else {
            return Self::Other;
        };
        if failure.classification == Some(FailureClassification::ContextOverflow) {
            return Self::Context;
        }
        if let Some(FailureClassification::UsageLimit { reset }) = failure.classification {
            return Self::UsageLimit { reset };
        }
        match &failure.class {
            ProviderFailureClass::Authentication => Self::Authentication,
            ProviderFailureClass::QuotaExceeded => Self::Quota,
            // A 400 that names no context overflow is the request shape being
            // refused, not proof that this Session can never be compacted: the
            // cold summary is a different shape and the next input may fit.
            // `Transient` retries on the next input instead of pausing
            // automatic compaction until the model, budget or a rewind changes.
            ProviderFailureClass::InvalidRequest => Self::Transient,
            class if class.is_retryable() => Self::Transient,
            _ => Self::Other,
        }
    }

    /// The cause of an attempt's terminal reason.
    ///
    /// [`Self::of`] reads the transport detail, which is absent for an attempt
    /// the Step's watchdog cancelled: its class travels in the terminal
    /// reason alone. Reading only the detail recorded a stalled summary as
    /// `Other`, a permanent pause, so the next input could not retry it. A
    /// slow prefill on a local route says nothing against the compaction, and
    /// the vocabulary already calls that class retry-eligible.
    pub(super) fn of_reason(reason: &grapt_types::AttemptTerminalReason) -> Self {
        use grapt_types::{AttemptTerminalReason, ProviderFailureClass as Terminal};
        match reason {
            AttemptTerminalReason::ProviderFailure { class } => match class {
                Terminal::Authentication => Self::Authentication,
                Terminal::Quota => Self::Quota,
                // Same reading as [`Self::of`]: a refused request shape says
                // nothing permanent about this Session's compaction.
                Terminal::InvalidRequest => Self::Transient,
                class if class.is_retry_eligible() => Self::Transient,
                _ => Self::Other,
            },
            AttemptTerminalReason::ContextFailure { .. } => Self::Context,
            // An interrupt is nobody's failure, and says nothing about this
            // compaction; the Step ends rather than pausing later ones.
            AttemptTerminalReason::Interrupted { .. } => Self::Transient,
            AttemptTerminalReason::ProviderEnded { .. } => Self::Other,
            AttemptTerminalReason::AuthorityRefused { .. }
            | AttemptTerminalReason::DurabilityFailure
            | AttemptTerminalReason::ProcessLoss { .. } => Self::Other,
        }
    }

    /// A summary call refused before it ran. A summary the full adapter kept
    /// waiting past its bound is tried again on the next input, never paused
    /// as a permanent failure (SC-REL-13).
    pub(super) fn of_refusal(refusal: &grapt_runtime::SummaryRefusal) -> Option<Self> {
        refusal.is_saturated().then_some(Self::Transient)
    }

    pub(in crate::dogfood) fn public(self) -> grapt_session::execution::CompactionRecovery {
        use grapt_session::execution::CompactionRecovery as Public;
        match self {
            Self::Context => Public::Context,
            Self::Authentication => Public::Authentication,
            Self::Quota => Public::Quota,
            Self::UsageLimit { reset } => Public::UsageLimit { reset },
            Self::Transient => Public::Transient,
            Self::Other => Public::Other,
            Self::NoProgress => Public::NoProgress,
            Self::Capacity => Public::Capacity,
        }
    }

    pub(super) fn message(self) -> String {
        self.public().message()
    }

    pub(super) fn blocks_fallback(self) -> bool {
        matches!(
            self,
            Self::Context
                | Self::Authentication
                | Self::Quota
                | Self::UsageLimit { .. }
                | Self::Other
        )
    }
}

#[derive(Clone)]
struct Target {
    model: ModelRef,
    account: AccountId,
    credential: CredentialVersion,
}

impl Target {
    fn of(target: &ResolvedModelTarget) -> Self {
        Self {
            model: target.model_ref().clone(),
            account: target.account().clone(),
            credential: target.credential_version().clone(),
        }
    }

    fn same_account(&self, other: &Self) -> bool {
        self.model.provider() == other.model.provider() && self.account == other.account
    }
}

#[derive(Clone, Copy, Default)]
struct Progress {
    compacted: Option<u64>,
    rewound: Option<u64>,
    answered: Option<u64>,
}

fn progress(inbox: &Inbox, session: &SessionId) -> Result<Progress, String> {
    let values: (Option<i64>, Option<i64>, Option<i64>) = inbox
        .store()
        .connection()
        .query_row(
            "SELECT MAX(CASE WHEN type = 'session.compaction.ended.2' THEN seq END),
                MAX(CASE WHEN type = 'session.revert.committed.1' THEN seq END),
                MAX(CASE WHEN type IN ('session.step.ended.2', 'session.step.ended.3') THEN seq END)
         FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
            [session.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|error| format!("compaction recovery observations are unreadable: {error}"))?;
    let seq = |value: Option<i64>| {
        value
            .map(|value| {
                u64::try_from(value)
                    .map_err(|_| "compaction recovery has a negative event sequence".to_owned())
            })
            .transpose()
    };
    Ok(Progress {
        compacted: seq(values.0)?,
        rewound: seq(values.1)?,
        answered: seq(values.2)?,
    })
}

#[derive(Clone)]
struct Suppressed {
    reported: bool,
    cause: Cause,
    target: Target,
    budget: Option<u64>,
    input: MessageId,
    observed: Progress,
}

/// Process-memory policy observations, not a retry worker or durable authority.
/// At capacity, new automatic calls refuse rather than evicting another Session's block.
#[derive(Clone, Default)]
pub(in crate::dogfood) struct Registry(Arc<Mutex<BTreeMap<SessionId, Suppressed>>>);

impl Registry {
    /// Consume only a failure the current execution's compactor reported.
    /// Optional threshold checks do not report a failure.
    pub(in crate::dogfood) fn take_failure(&self, session: &SessionId) -> Option<Cause> {
        let mut sessions = lock(&self.0);
        let previous = sessions.get_mut(session)?;
        let reported = std::mem::take(&mut previous.reported);
        reported.then_some(previous.cause)
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompactionRecovery([REDACTED])")
    }
}

#[derive(Clone)]
pub(super) struct Session {
    registry: Registry,
    session: SessionId,
    input: MessageId,
    target: Arc<Mutex<Target>>,
    budget: Option<u64>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompactionRecoverySession([REDACTED])")
    }
}

impl Session {
    pub(super) fn new(
        registry: Registry,
        session: SessionId,
        input: MessageId,
        target: &ResolvedModelTarget,
        budget: Option<u64>,
    ) -> Self {
        Self {
            registry,
            session,
            input,
            target: Arc::new(Mutex::new(Target::of(target))),
            budget,
        }
    }

    pub(super) fn promote_input(&mut self, input: &MessageId) {
        self.input = input.clone();
    }

    pub(super) fn select(&self, target: &ResolvedModelTarget) {
        *lock(&self.target) = Target::of(target);
    }

    pub(super) fn trim_requested(&self) -> bool {
        lock(&self.registry.0)
            .get(&self.session)
            .is_some_and(|previous| {
                previous.cause == Cause::NoProgress && self.input != previous.input
            })
    }

    pub(super) fn blocked(&self, inbox: &Inbox) -> Result<Option<Cause>, String> {
        let previous = {
            let sessions = lock(&self.registry.0);
            match sessions.get(&self.session) {
                Some(previous) => previous.clone(),
                None => {
                    return Ok((sessions
                        .values()
                        .filter(|entry| entry.cause != Cause::NoProgress)
                        .count()
                        >= MAX_SUPPRESSED_SESSIONS)
                        .then_some(Cause::Capacity))
                }
            }
        };
        // A fresh input may use a deterministic reduced request. Keep the
        // degraded-mode observation until an actual compaction succeeds.
        if previous.cause == Cause::NoProgress && self.input != previous.input {
            return Ok(None);
        }
        let observed = progress(inbox, &self.session)?;
        let target = lock(&self.target);
        let recovered = observed.compacted > previous.observed.compacted
            || match previous.cause {
                Cause::Context | Cause::Other => {
                    previous.target.model != target.model
                        || self.budget != previous.budget
                        || observed.rewound > previous.observed.rewound
                }
                Cause::Authentication => {
                    !previous.target.same_account(&target)
                        || previous.target.credential != target.credential
                }
                Cause::Quota | Cause::UsageLimit { .. } => {
                    !previous.target.same_account(&target)
                        || observed.answered > previous.observed.answered
                }
                Cause::Transient => self.input != previous.input,
                Cause::NoProgress => false,
                Cause::Capacity => false,
            };
        drop(target);
        if recovered {
            self.succeeded();
            Ok(None)
        } else {
            Ok(Some(previous.cause))
        }
    }

    pub(super) fn failed(&self, cause: Cause, inbox: &Inbox) -> Result<(), String> {
        let observed = progress(inbox, &self.session)?;
        let mut sessions = lock(&self.registry.0);
        if !sessions.contains_key(&self.session) && sessions.len() >= MAX_SUPPRESSED_SESSIONS {
            // Forgetting a degraded projection may cost one more summary;
            // it cannot re-enable an authentication, quota or authority block.
            let disposable = sessions
                .iter()
                .find(|(_, entry)| entry.cause == Cause::NoProgress)
                .map(|(session, _)| session.clone());
            if let Some(session) = disposable {
                sessions.remove(&session);
            }
        }
        if sessions.contains_key(&self.session) || sessions.len() < MAX_SUPPRESSED_SESSIONS {
            sessions.insert(
                self.session.clone(),
                Suppressed {
                    reported: false,
                    cause,
                    target: lock(&self.target).clone(),
                    budget: self.budget,
                    input: self.input.clone(),
                    observed,
                },
            );
        }
        Ok(())
    }

    pub(super) fn report_failure(&self) {
        if let Some(previous) = lock(&self.registry.0).get_mut(&self.session) {
            previous.reported = true;
        }
    }

    pub(super) fn succeeded(&self) {
        lock(&self.registry.0).remove(&self.session);
    }
}

#[cfg(test)]
#[path = "compaction_recovery_tests.rs"]
mod tests;
