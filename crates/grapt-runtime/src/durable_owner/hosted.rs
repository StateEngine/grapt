//! Provider-hosted output uses the existing artifact capture boundary. The
//! supervisor commits the returned reference through the ordinary terminal.
use super::*;

impl<A: ArtifactStore, S: CommittedFactSink> DurableAttemptOwner<'_, A, S> {
    pub(super) fn capture_hosted(
        &mut self,
        text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, OwnerError> {
        let bound = CaptureBound::new(
            crate::TOOL_ARTIFACT_MAX_BYTES,
            crate::TOOL_ARTIFACT_BUFFER_BYTES,
        )
        .ok_or_else(|| OwnerError {
            reason: "hosted output capture bound is invalid".into(),
        })?;
        let captured = self
            .artifacts
            .capture(
                text.as_bytes(),
                bound,
                ArtifactKind::CapturedOutput,
                Sensitivity::SecretBearing,
            )
            .map_err(|_| OwnerError {
                reason: "hosted output artifact capture failed".into(),
            })?;
        grapt_types::ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
            .ok_or_else(|| OwnerError {
                reason: "hosted output capture disposition disagrees".into(),
            })
    }

    pub(super) fn capture_diagnostic(
        &mut self,
        text: &str,
    ) -> Result<grapt_types::ToolTerminalArtifact, OwnerError> {
        let bound = CaptureBound::new(2048, 2048).ok_or_else(|| OwnerError {
            reason: "diagnostic detail capture bound is invalid".into(),
        })?;
        let captured = self
            .artifacts
            .capture(
                text.as_bytes(),
                bound,
                ArtifactKind::Diagnostic,
                Sensitivity::Ordinary,
            )
            .map_err(|error| OwnerError {
                reason: format!("diagnostic detail artifact capture failed: {error}"),
            })?;
        grapt_types::ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
            .ok_or_else(|| OwnerError {
                reason: "diagnostic detail capture disposition disagrees".into(),
            })
    }
}
