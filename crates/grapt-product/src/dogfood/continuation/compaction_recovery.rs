//! Why a compaction failed, and the last failure each Session's compactor reported.
//!
//! Copyright 2023-2026 SpaceXAI. Modified by Grapt: typed causes, bounded passive
//! session state, existing target identities and durable recovery observations.

use super::*;
use grapt_provider::{FailureClassification, ProviderFailureClass};
use grapt_runtime::supervisor::ProviderFailureDetail;
use std::collections::BTreeMap;

const MAX_SESSIONS: usize = 256;

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
    DidNotFit,
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
            Err(CompactionFailure::DidNotFit { .. }) => Self::DidNotFit,
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
            // `Transient` names it as retryable on the next input.
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
    /// `Other`, which names it as a permanent failure. A
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
            // compaction; the Step ends and the next input retries.
            AttemptTerminalReason::Interrupted { .. } => Self::Transient,
            AttemptTerminalReason::ProviderEnded { .. } => Self::Other,
            AttemptTerminalReason::AuthorityRefused { .. }
            | AttemptTerminalReason::DurabilityFailure
            | AttemptTerminalReason::ProcessLoss { .. } => Self::Other,
        }
    }

    /// A summary call refused before it ran. A summary the full adapter kept
    /// waiting past its bound is named retryable, never a permanent failure.
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
            Self::DidNotFit => Public::DidNotFit,
        }
    }

    pub(super) fn message(self) -> String {
        self.public().message()
    }

    fn trims(self) -> bool {
        matches!(self, Self::NoProgress | Self::DidNotFit)
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

/// The last failure a Session's compaction ended in, and the input it failed
/// on: `None` when the runtime, not this compactor, decided it.
#[derive(Clone)]
struct Last {
    reported: bool,
    cause: Cause,
    input: Option<MessageId>,
}

/// Process-memory observations, not a retry worker or durable authority:
/// nothing here stops a compaction. Forgetting a Session at capacity costs at
/// most one untrimmed retry and the generic failure message.
#[derive(Clone, Default)]
pub(in crate::dogfood) struct Registry(Arc<Mutex<BTreeMap<SessionId, Last>>>);

impl Registry {
    /// Consume only a failure the current execution's compactor reported.
    /// Optional threshold checks do not report a failure.
    pub(in crate::dogfood) fn take_failure(&self, session: &SessionId) -> Option<Cause> {
        let mut sessions = lock(&self.0);
        let previous = sessions.get_mut(session)?;
        let reported = std::mem::take(&mut previous.reported);
        reported.then_some(previous.cause)
    }

    /// The Step ended because the context a compaction left still did not
    /// fit, decided after the compaction itself succeeded (a provider-native
    /// or structural one). It is named as did-not-fit, and the next input
    /// runs trimmed, exactly as when the compactor's own guard decides it.
    pub(in crate::dogfood) fn did_not_fit(&self, session: &SessionId) -> Cause {
        insert(
            &mut lock(&self.0),
            session,
            Last {
                reported: false,
                cause: Cause::DidNotFit,
                input: None,
            },
        );
        Cause::DidNotFit
    }
}

fn insert(sessions: &mut BTreeMap<SessionId, Last>, session: &SessionId, last: Last) {
    if !sessions.contains_key(session) && sessions.len() >= MAX_SESSIONS {
        sessions.pop_first();
    }
    sessions.insert(session.clone(), last);
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
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CompactionRecoverySession([REDACTED])")
    }
}

impl Session {
    pub(super) fn new(registry: Registry, session: SessionId, input: MessageId) -> Self {
        Self {
            registry,
            session,
            input,
        }
    }

    pub(super) fn promote_input(&mut self, input: &MessageId) {
        self.input = input.clone();
    }

    /// A compaction that left no usable context ended the last input; a new
    /// input runs on a visibly trimmed context instead of the same summary.
    pub(super) fn trim_requested(&self) -> bool {
        lock(&self.registry.0)
            .get(&self.session)
            .is_some_and(|previous| {
                previous.cause.trims() && previous.input.as_ref() != Some(&self.input)
            })
    }

    pub(super) fn failed(&self, cause: Cause) {
        insert(
            &mut lock(&self.registry.0),
            &self.session,
            Last {
                reported: false,
                cause,
                input: Some(self.input.clone()),
            },
        );
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
