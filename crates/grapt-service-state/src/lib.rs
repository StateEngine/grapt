//! Journaled observations of the execution owner's session state.
//!
//! These records describe an owner; replay never resumes one.
use grapt_session::execution;
use grapt_session::CommittedPosition;
use grapt_types::ExecutionId;
use serde::{Deserialize, Serialize};

mod journal;
pub mod lifecycle;
mod reader;
mod recovery;
pub use journal::{record, Change};
pub use reader::read;
pub use recovery::recover;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Idle,
    Running,
    Waiting,
    Paused,
    Failed,
    Interrupted,
    Disconnected,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    Preparing,
    Compacting,
    ProviderSlot,
    ProviderRetry,
    Permission,
    Question,
    Compaction,
    Capability,
    Continuity,
    Execution,
    User,
    Shutdown,
    Superseded,
    WindowClosed,
    ProcessLoss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    FollowUp,
    Compact,
    SignIn,
    RestoreQuota,
    Rebind,
    InspectConnection,
    InspectSession,
    ReviewPermission,
    AnswerQuestion,
    Wait,
    ChangeModel,
}

/// The owner supplies the cause and permitted recovery, never a consumer guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    #[serde(rename = "type")]
    pub kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<Reason>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub action: Option<Action>,
}

impl State {
    pub(crate) fn observed(
        settlement: Option<&(execution::Outcome, execution::Failure)>,
        interruption: Option<execution::InterruptionReason>,
    ) -> Self {
        match settlement {
            None => Self::plain(Kind::Running),
            Some((execution::Outcome::Succeeded, _)) => Self::plain(Kind::Completed),
            Some((execution::Outcome::Failed, failure)) => Self::failed(failure),
            Some((execution::Outcome::Interrupted, _)) => {
                let (reason, message) = match interruption {
                    Some(execution::InterruptionReason::Shutdown) => (
                        Reason::Shutdown,
                        "The service stopped this execution; work so far is kept.",
                    ),
                    Some(execution::InterruptionReason::Superseded) => (
                        Reason::Superseded,
                        "A newer owner replaced this execution; work so far is kept.",
                    ),
                    Some(execution::InterruptionReason::ServeWindowExpired) => (
                        Reason::WindowClosed,
                        "The last serving window closed; work so far is kept.",
                    ),
                    _ => (
                        Reason::User,
                        "The execution was interrupted; work so far is kept.",
                    ),
                };
                Self::described(Kind::Interrupted, reason, message, Some(Action::FollowUp))
            }
        }
    }

    pub fn plain(kind: Kind) -> Self {
        Self {
            kind,
            reason: None,
            message: None,
            action: None,
        }
    }

    pub fn described(kind: Kind, reason: Reason, message: &str, action: Option<Action>) -> Self {
        let mut bounded = String::with_capacity(message.len().min(1024));
        for value in message.chars() {
            let value = if value.is_control() { ' ' } else { value };
            if bounded.len() + value.len_utf8() > 1024 {
                while bounded.len() > 1021 {
                    bounded.pop();
                }
                bounded.push('…');
                break;
            }
            bounded.push(value);
        }
        Self {
            kind,
            reason: Some(reason),
            message: Some(bounded),
            action,
        }
    }

    pub fn active(&self) -> bool {
        matches!(self.kind, Kind::Running | Kind::Waiting)
    }

    fn validate(&self) -> Result<(), String> {
        if self.message.as_ref().is_some_and(|message| {
            message.is_empty() || message.len() > 1024 || message.chars().any(char::is_control)
        }) || (matches!(
            self.kind,
            Kind::Waiting | Kind::Paused | Kind::Failed | Kind::Disconnected
        ) && (self.reason.is_none() || self.message.is_none()))
        {
            return Err("session status has an invalid cause or message".into());
        }
        Ok(())
    }

    pub fn failed(failure: &execution::Failure) -> Self {
        use execution::{
            CompactionRecovery as Compaction, ContinuityFailure as Continuity, Failure,
        };
        let message = failure.public().message;
        match failure {
            Failure::CompactionRecovery(cause) => {
                let action = match cause {
                    Compaction::Authentication => Action::SignIn,
                    Compaction::Quota => Action::RestoreQuota,
                    Compaction::UsageLimit { .. } => Action::SignIn,
                    Compaction::Transient | Compaction::NoProgress | Compaction::DidNotFit => {
                        Action::FollowUp
                    }
                    Compaction::Context | Compaction::Other => Action::ChangeModel,
                };
                Self::described(Kind::Paused, Reason::Compaction, &message, Some(action))
            }
            Failure::Continuity(Continuity::Capability | Continuity::Principal) => Self::described(
                Kind::Paused,
                Reason::Capability,
                &message,
                Some(Action::Rebind),
            ),
            Failure::Continuity(_) => Self::described(
                Kind::Paused,
                Reason::Continuity,
                &message,
                Some(Action::InspectConnection),
            ),
            Failure::Generic => Self::described(
                Kind::Failed,
                Reason::Execution,
                &message,
                Some(Action::InspectSession),
            ),
            Failure::ProviderLimit { .. } => Self::described(
                Kind::Failed,
                Reason::Execution,
                &message,
                Some(Action::SignIn),
            ),
            // A provider refusal names its own cause in the message, so the
            // useful action is to look at it rather than to resend the same
            // request; a rejected request fails the same way twice.
            Failure::ProviderFailed(_) => Self::described(
                Kind::Failed,
                Reason::Execution,
                &message,
                Some(Action::InspectSession),
            ),
            _ => Self::described(
                Kind::Failed,
                Reason::Execution,
                &message,
                Some(Action::FollowUp),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Its committed sequence is the revision, including across process restart.
    pub position: CommittedPosition,
    pub execution: Option<ExecutionId>,
    pub state: State,
}
