//! Tool definition identity and schema version. Law (foundational report row
//! `protocol`/`tool`): the Rust argument/request type is authoritative and the
//! provider/JSON tool schema is a generated, checked projection of it. A tool
//! definition therefore carries a stable id and an explicit schema version the
//! generated projection is validated against — never a separately hand-authored
//! competing schema (compile-fail probe #10).

crate::id::opaque_id! {
    /// A stable tool-definition identity. The authoritative argument type is
    /// bound to this id; a generated provider/JSON schema references it.
    ToolDefinitionId
}

crate::id::opaque_id! {
    /// The explicit version of a tool's authoritative Rust argument/request
    /// schema. A generated provider schema is a checked projection at this exact
    /// version, never an independently versioned competing truth.
    ToolSchemaVersion
}

use crate::{ArtifactRef, CaptureDisposition, InterruptionSource};

/// Who performs a tool call. Historical v1 input identities are local; new
/// identities preserve this choice so hosted history cannot become a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolExecution {
    Local,
    ProviderHosted,
}

/// One artifact-backed tool payload, with its capture disposition kept beside
/// the reference it qualifies.
///
/// The constructor refuses a truncated disposition whose captured byte count
/// disagrees with the reference length. This prevents a decoder or projection
/// from pairing independently decoded halves into a false terminal fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolTerminalArtifact {
    reference: ArtifactRef,
    disposition: CaptureDisposition,
}

impl ToolTerminalArtifact {
    pub fn new(reference: ArtifactRef, disposition: CaptureDisposition) -> Option<Self> {
        if let CaptureDisposition::TruncatedAtBoundary { captured } = disposition {
            if captured != reference.length() {
                return None;
            }
        }
        Some(Self {
            reference,
            disposition,
        })
    }

    pub fn reference(&self) -> &ArtifactRef {
        &self.reference
    }

    pub fn disposition(&self) -> CaptureDisposition {
        self.disposition
    }
}

/// Closed failure classification for a tool terminal.
///
/// Detail, when retained, is a separately bounded artifact on
/// [`ToolTerminalOutcome::Failed`]. No failure string is durable control flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolFailureReason {
    /// No authenticated live authority issuer was available for this call.
    AuthorityUnavailable,
    /// Policy requires approval, but no matching approver is attached.
    ApprovalUnavailable,
    /// The governed executor reported failure.
    EffectFailed,
    /// Bounded artifact capture could not commit the executor's result/detail.
    CaptureFailed,
    /// Custody ended before the external effect's result committed. The
    /// Session continues with a notice to verify that state; never an
    /// automatic retry signal.
    EffectUnknown,
    /// Runtime, supervisor, or local worker infrastructure failed before the
    /// governed effect could run or report a terminal outcome.
    InfrastructureFailed,
}

/// Exactly one durable terminal outcome for a committed tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolTerminalOutcome {
    /// The effect completed. `None` is an intentionally absent output; an empty
    /// output is `Some` with a zero-length artifact and remains distinguishable.
    Completed {
        output: Option<ToolTerminalArtifact>,
    },
    /// The effect failed for a typed reason. Optional detail is artifact-backed.
    Failed {
        reason: ToolFailureReason,
        detail: Option<ToolTerminalArtifact>,
    },
    /// The owner cancelled the call and records who initiated cancellation.
    Cancelled { by: InterruptionSource },
    /// A provider-hosted call reached stream EOF without a result or failure.
    ResultMissing,
}

impl ToolTerminalOutcome {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::{ToolDefinitionId, ToolSchemaVersion, ToolTerminalArtifact};
    use crate::{ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition};

    #[test]
    fn tool_identity_and_schema_version_are_non_empty() {
        assert!(ToolDefinitionId::new("edit").is_some());
        assert!(ToolSchemaVersion::new("v1").is_some());
        assert!(ToolDefinitionId::new("").is_none());
        assert!(ToolSchemaVersion::new("").is_none());
    }

    #[test]
    fn terminal_artifact_refuses_a_false_truncation_pair() {
        let id = ArtifactId::new("art_tool").unwrap_or_else(|| panic!("valid artifact id"));
        let digest = ArtifactDigest::new("sha256-tool").unwrap_or_else(|| panic!("valid digest"));
        let reference = ArtifactRef::new(id, 3, digest);
        assert!(ToolTerminalArtifact::new(
            reference.clone(),
            CaptureDisposition::TruncatedAtBoundary { captured: 2 }
        )
        .is_none());
        assert!(ToolTerminalArtifact::new(
            reference,
            CaptureDisposition::TruncatedAtBoundary { captured: 3 }
        )
        .is_some());
    }
}
