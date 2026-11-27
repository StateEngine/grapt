use std::sync::Arc;

use grapt_artifact::{
    ArtifactKind, ArtifactStore, CaptureBound, FilesystemArtifactStore, Sensitivity,
};
use grapt_runtime::BoxFuture;
use grapt_types::{ToolFailureReason, ToolTerminalArtifact, ToolTerminalOutcome};

pub(super) const EARLY_FAILURE_DETAIL_BYTES: usize = 2 * 1024;
const EARLY_FAILURE_DETAIL_BYTES_U64: u64 = 2 * 1024;

fn safe_detail(detail: &str) -> String {
    let mut safe = String::with_capacity(detail.len().min(EARLY_FAILURE_DETAIL_BYTES));
    for character in detail.chars() {
        let character = if character.is_control() && character != '\n' {
            ' '
        } else {
            character
        };
        if safe.len().saturating_add(character.len_utf8()) > EARLY_FAILURE_DETAIL_BYTES {
            break;
        }
        safe.push(character);
    }
    if safe.trim().is_empty() {
        "tool request failed before execution".to_owned()
    } else {
        safe
    }
}

pub(super) fn captured_failure(
    artifacts: &FilesystemArtifactStore,
    reason: ToolFailureReason,
    detail: &str,
) -> ToolTerminalOutcome {
    let detail = safe_detail(detail);
    let Some(bound) = CaptureBound::new(EARLY_FAILURE_DETAIL_BYTES_U64, EARLY_FAILURE_DETAIL_BYTES)
    else {
        return ToolTerminalOutcome::Failed {
            reason,
            detail: None,
        };
    };
    let detail = artifacts
        .capture(
            std::io::Cursor::new(detail.into_bytes()),
            bound,
            ArtifactKind::CapturedOutput,
            Sensitivity::Ordinary,
        )
        .ok()
        .and_then(|captured| {
            ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
        });
    ToolTerminalOutcome::Failed { reason, detail }
}

pub(super) fn failed_with_detail(
    artifacts: Arc<FilesystemArtifactStore>,
    reason: ToolFailureReason,
    detail: String,
) -> BoxFuture<'static, ToolTerminalOutcome> {
    Box::pin(async move { captured_failure(&artifacts, reason, &detail) })
}
