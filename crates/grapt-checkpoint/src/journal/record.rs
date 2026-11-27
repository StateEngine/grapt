//! Validate the bounded on-disk journal vocabulary.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(super) enum DurableLine {
    Applying(Box<DurableApplying>),
    Terminal(DurableTerminal),
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct DurableApplying {
    schema: u8,
    operation: String,
    plan: String,
    provider: String,
    version: String,
    checkpoint: String,
    root_kind: String,
    root: String,
    common_dir: Option<String>,
    git_dir: Option<String>,
    lanes: Vec<String>,
    permission: String,
    admitted_at_ms: i64,
    writes: u64,
    deletes: u64,
    bytes: u64,
}

impl From<&ApplyingRestore> for DurableApplying {
    fn from(value: &ApplyingRestore) -> Self {
        Self {
            schema: 1,
            operation: value.operation.clone(),
            plan: value.plan.clone(),
            provider: value.provider.clone(),
            version: value.version.clone(),
            checkpoint: value.checkpoint.clone(),
            root_kind: match value.root_kind {
                RootKind::Git => "git",
                RootKind::Plain => "plain",
            }
            .to_owned(),
            root: value.root.clone(),
            common_dir: value.common_dir.clone(),
            git_dir: value.git_dir.clone(),
            lanes: value.lanes.clone(),
            permission: value.permission.clone(),
            admitted_at_ms: value.admitted_at_ms,
            writes: value.writes,
            deletes: value.deletes,
            bytes: value.bytes,
        }
    }
}

impl TryFrom<DurableApplying> for ApplyingRestore {
    type Error = JournalError;

    fn try_from(value: DurableApplying) -> Result<Self, Self::Error> {
        if value.schema != 1 {
            return Err(JournalError::Corrupt);
        }
        let root_kind = match value.root_kind.as_str() {
            "git" => RootKind::Git,
            "plain" => RootKind::Plain,
            _ => return Err(JournalError::Corrupt),
        };
        let record = Self {
            operation: value.operation,
            plan: value.plan,
            provider: value.provider,
            version: value.version,
            checkpoint: value.checkpoint,
            root_kind,
            root: value.root,
            common_dir: value.common_dir,
            git_dir: value.git_dir,
            lanes: value.lanes,
            permission: value.permission,
            admitted_at_ms: value.admitted_at_ms,
            writes: value.writes,
            deletes: value.deletes,
            bytes: value.bytes,
        };
        validate_applying(&record)?;
        Ok(record)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct DurableTerminal {
    schema: u8,
    state: String,
    refusal: Option<String>,
}

impl From<&TerminalRestore> for DurableTerminal {
    fn from(value: &TerminalRestore) -> Self {
        let (state, refusal) = match &value.state {
            ReconciledState::Applied => ("applied", None),
            ReconciledState::Quarantined => ("quarantined", None),
            ReconciledState::Refused(reason) => ("refused", Some(refusal_name(reason))),
        };
        Self {
            schema: 1,
            state: state.to_owned(),
            refusal: refusal.map(str::to_owned),
        }
    }
}

impl TryFrom<DurableTerminal> for TerminalRestore {
    type Error = JournalError;

    fn try_from(value: DurableTerminal) -> Result<Self, Self::Error> {
        if value.schema != 1 {
            return Err(JournalError::Corrupt);
        }
        let state = match (value.state.as_str(), value.refusal.as_deref()) {
            ("applied", None) => ReconciledState::Applied,
            ("quarantined", None) => ReconciledState::Quarantined,
            ("refused", Some(reason)) => ReconciledState::Refused(parse_refusal(reason)?),
            _ => return Err(JournalError::Corrupt),
        };
        Ok(Self { state })
    }
}

fn refusal_name(reason: &ProviderRefusal) -> &'static str {
    match reason {
        ProviderRefusal::Unavailable => "unavailable",
        ProviderRefusal::VersionMismatch => "version-mismatch",
        ProviderRefusal::WrongRoot => "wrong-root",
        ProviderRefusal::CheckpointUnknown => "checkpoint-unknown",
        ProviderRefusal::HeadUnknown => "head-unknown",
        ProviderRefusal::LaneUnknown => "lane-unknown",
        ProviderRefusal::PreconditionsChanged => "preconditions-changed",
        ProviderRefusal::OperationUnknown => "operation-unknown",
        ProviderRefusal::Conflict => "conflict",
    }
}

fn parse_refusal(raw: &str) -> Result<ProviderRefusal, JournalError> {
    match raw {
        "unavailable" => Ok(ProviderRefusal::Unavailable),
        "version-mismatch" => Ok(ProviderRefusal::VersionMismatch),
        "wrong-root" => Ok(ProviderRefusal::WrongRoot),
        "checkpoint-unknown" => Ok(ProviderRefusal::CheckpointUnknown),
        "head-unknown" => Ok(ProviderRefusal::HeadUnknown),
        "lane-unknown" => Ok(ProviderRefusal::LaneUnknown),
        "preconditions-changed" => Ok(ProviderRefusal::PreconditionsChanged),
        "operation-unknown" => Ok(ProviderRefusal::OperationUnknown),
        "conflict" => Ok(ProviderRefusal::Conflict),
        _ => Err(JournalError::Corrupt),
    }
}
