//! Subagent operation vocabulary — the corrected LAW, not the oracle table
//! shape.
//!
//! The oracle (`core/src/subagent/sql.ts`) keys a subagent operation by the
//! CHILD session id and stores a generic status string. Red-team B.2 and the
//! frozen fixture's `waivers.json` both record the correction: a subagent
//! operation has its OWN stable identity — [`grapt_types::SubagentOperationId`]
//! — distinct from the child [`grapt_types::SessionId`], and the domain
//! records BOTH so a child-session migration cannot impersonate an operation.
//!
//! The lifecycle is the shared operation LAW (`grapt_types::operation`):
//! `Admitted -> Running(generation) -> DomainTerminal | ExternallyAmbiguous`,
//! with `ExternallyAmbiguous` reconciled to a terminal (never auto-replayed).
//! Terminality and delivery are independent facts. Each state carries exactly
//! its payload — there is no generic status with a nullable-field bag.

use grapt_types::{
    AgentId, AmbiguityEvidence, ArtifactRef, CaptureDisposition, DeliveryClaimId, EffectiveGraphId,
    MessageId, ModelRef, OperationId, RuntimeGeneration, SessionId, SubagentOperationId,
    ToolCallId,
};

use grapt_artifact::{ArtifactKind, ArtifactStore, FetchError, ReadBudget, Verified};

/// How the subagent runs. The mode determines the only legal delivery route:
/// a background operation delivers a typed operation fact to the parent; a
/// foreground operation delivers a tool result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentMode {
    Background,
    Foreground,
}

impl SubagentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SubagentMode::Background => "background",
            SubagentMode::Foreground => "foreground",
        }
    }

    pub fn from_label(raw: &str) -> Option<Self> {
        match raw {
            "background" => Some(SubagentMode::Background),
            "foreground" => Some(SubagentMode::Foreground),
            _ => None,
        }
    }

    /// The only delivery route legal for this mode.
    pub fn route(self) -> ClaimRoute {
        match self {
            SubagentMode::Background => ClaimRoute::OperationDelivery,
            SubagentMode::Foreground => ClaimRoute::ToolResult,
        }
    }
}

/// Why a subagent operation was cancelled. Closed: a running operation from a
/// dead runtime generation settles `ProcessLoss` and is never replayed;
/// `Interrupted` is an explicit caller cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentCancelReason {
    ProcessLoss,
    Interrupted,
}

impl SubagentCancelReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SubagentCancelReason::ProcessLoss => "process-loss",
            SubagentCancelReason::Interrupted => "interrupted",
        }
    }

    pub fn from_label(raw: &str) -> Option<Self> {
        match raw {
            "process-loss" => Some(SubagentCancelReason::ProcessLoss),
            "interrupted" => Some(SubagentCancelReason::Interrupted),
            _ => None,
        }
    }
}

/// The kind-owned domain-terminal payload. Terminal does NOT imply delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentTerminal {
    Completed { output: ArtifactBackedOutput },
    Failed { error: String },
    Cancelled(SubagentCancelReason),
}

impl SubagentTerminal {
    /// The `status` column vocabulary (matches the frozen oracle rows).
    pub fn status_str(&self) -> &'static str {
        match self {
            SubagentTerminal::Completed { .. } => "completed",
            SubagentTerminal::Failed { .. } => "failed",
            SubagentTerminal::Cancelled(_) => "cancelled",
        }
    }

    /// The child-facing state label for the rendered delivery.
    pub fn state_label(&self) -> &'static str {
        self.status_str()
    }
}

/// Which delivery route claimed the terminal outcome. Bound to the mode: a
/// claim whose route does not match the operation's mode is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimRoute {
    /// Background completion delivers a typed operation fact to the parent.
    OperationDelivery,
    /// Foreground completion delivers a tool result.
    ToolResult,
}

impl ClaimRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            // Storage spelling retained for compatibility with the accepted
            // pre-R3 table; the inbox fact itself is typed operation delivery.
            ClaimRoute::OperationDelivery => "synthetic",
            ClaimRoute::ToolResult => "tool-result",
        }
    }

    pub fn from_label(raw: &str) -> Option<Self> {
        match raw {
            "synthetic" => Some(ClaimRoute::OperationDelivery),
            "tool-result" => Some(ClaimRoute::ToolResult),
            _ => None,
        }
    }
}

/// The irreducible admission facts of a subagent operation — all immutable
/// after `open`. The parent session is the operation's owner route; the
/// effective graph is a REQUIRED admission fact (authority is irreducible).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentAdmission {
    /// Shared operation-family identity. It never substitutes for the more
    /// specific subagent address below.
    pub operation: OperationId,
    /// Durable subagent address, distinct from [`OperationId`].
    pub subagent: SubagentOperationId,
    pub child: SessionId,
    pub parent: SessionId,
    pub agent: AgentId,
    pub model: Option<ModelRef>,
    pub description: String,
    pub mode: SubagentMode,
    pub tool_call: Option<ToolCallId>,
    pub effective_graph: EffectiveGraphId,
}

/// A delivery claim: a typed claim identity bound to the EXACT delivery
/// coordinates it intends. The claim is the exactly-once arbiter, and later
/// delivery and reopen must present the same claim identity, parent, and input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryClaim {
    pub id: DeliveryClaimId,
    pub route: ClaimRoute,
    pub parent: SessionId,
    pub input: MessageId,
    /// The exact run generation whose terminal result this claim delivers.
    pub generation: RuntimeGeneration,
}

/// Verified, bounded result stored once as an artifact plus a smaller
/// model-facing projection. Construction verifies the complete artifact bytes;
/// restart decode may only reconstruct values that the service already
/// committed through that verified boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactBackedOutput {
    artifact: ArtifactRef,
    disposition: CaptureDisposition,
    projection: String,
}

#[derive(Debug, thiserror::Error)]
pub enum OutputVerificationError {
    #[error("the result projection is {actual} bytes, above the {maximum}-byte bound")]
    ProjectionTooLarge { actual: u64, maximum: u64 },
    #[error("the result artifact metadata is unavailable or does not match the reference")]
    MetadataMismatch,
    #[error("the result artifact is not captured output")]
    WrongKind,
    #[error("the result artifact could not be verified: {0}")]
    Fetch(#[from] FetchError),
    #[error("the artifact read budget cannot represent this artifact")]
    InvalidReadBudget,
    #[error("the artifact verification did not cover the whole artifact")]
    PartialVerification,
}

impl ArtifactBackedOutput {
    pub fn verify<S: ArtifactStore>(
        store: &S,
        artifact: ArtifactRef,
        disposition: CaptureDisposition,
        projection: String,
        max_projection_bytes: u64,
    ) -> Result<Self, OutputVerificationError> {
        let actual = u64::try_from(projection.len()).map_err(|_| {
            OutputVerificationError::ProjectionTooLarge {
                actual: u64::MAX,
                maximum: max_projection_bytes,
            }
        })?;
        if actual > max_projection_bytes {
            return Err(OutputVerificationError::ProjectionTooLarge {
                actual,
                maximum: max_projection_bytes,
            });
        }
        let metadata = store
            .stat(artifact.id())
            .map_err(|_| OutputVerificationError::MetadataMismatch)?;
        if metadata.reference() != artifact || metadata.disposition() != disposition {
            return Err(OutputVerificationError::MetadataMismatch);
        }
        if metadata.kind() != ArtifactKind::CapturedOutput {
            return Err(OutputVerificationError::WrongKind);
        }
        let budget = ReadBudget::new(artifact.length().max(1))
            .ok_or(OutputVerificationError::InvalidReadBudget)?;
        let page = store.fetch(&artifact, 0, budget)?;
        if page.verified() != Verified::Whole || page.next_offset().is_some() {
            return Err(OutputVerificationError::PartialVerification);
        }
        Ok(Self {
            artifact,
            disposition,
            projection,
        })
    }

    pub fn artifact(&self) -> &ArtifactRef {
        &self.artifact
    }

    pub fn disposition(&self) -> CaptureDisposition {
        self.disposition
    }

    pub fn projection(&self) -> &str {
        &self.projection
    }

    pub(crate) fn from_durable(
        artifact: ArtifactRef,
        disposition: CaptureDisposition,
        projection: String,
    ) -> Self {
        Self {
            artifact,
            disposition,
            projection,
        }
    }
}

/// Atomic parent-inbox capacity. Both dimensions are checked inside the same
/// immediate transaction that appends the delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryLimits {
    max_pending: u64,
    max_projection_bytes: u64,
}

impl DeliveryLimits {
    pub fn new(max_pending: u64, max_projection_bytes: u64) -> Option<Self> {
        if max_pending == 0 || max_projection_bytes == 0 {
            return None;
        }
        Some(Self {
            max_pending,
            max_projection_bytes,
        })
    }

    pub fn max_pending(self) -> u64 {
        self.max_pending
    }

    pub fn max_projection_bytes(self) -> u64 {
        self.max_projection_bytes
    }
}

/// The live durable status of an operation row, read back after strict decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStatus {
    Admitted,
    Running(RuntimeGeneration),
    Terminal(SubagentTerminal),
    ExternallyAmbiguous(AmbiguityEvidence),
}

/// The delivery status of an operation row, after strict decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryStatus {
    Unclaimed,
    Delivering(DeliveryClaimId, ClaimRoute),
    Delivered(DeliveryClaimId, ClaimRoute),
    Failed(DeliveryClaimId, ClaimRoute),
    Quarantined(DeliveryClaimId, ClaimRoute),
    Acknowledged(DeliveryClaimId, ClaimRoute, DeliverySettlement),
    Cleaned(DeliveryClaimId, ClaimRoute, DeliverySettlement),
}

/// The durable outcome of parent notification. It stays attached through
/// acknowledgment and cleanup so failure and quarantine never collapse into a
/// generic "delivered" bit after restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliverySettlement {
    Completed,
    Failed,
    Quarantined,
}
