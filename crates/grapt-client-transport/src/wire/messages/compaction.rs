//! Preserve public compaction facts instead of identifying them by rendered text.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionPhase {
    Running,
    Completed,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compaction {
    pub id: MessageWireId,
    pub phase: CompactionPhase,
}
pub(super) fn decode(
    row: &grapt_api::u3a::SessionMessageCompactionDto,
) -> Result<DurableMessage, WireError> {
    use grapt_api::u3a::SessionMessageCompactionDto as C;
    let (id, phase, mut text) = match row {
        C::Completed(row) => (
            row.id().get().clone(),
            CompactionPhase::Completed,
            format!(
                "Compaction completed.\n### Summary\n\n{}",
                row.summary().get().as_str()
            ),
        ),
        C::Running(row) => (
            row.id().get().clone(),
            CompactionPhase::Running,
            "Compaction running.".into(),
        ),
        C::Failed(row) => (
            row.id().get().clone(),
            CompactionPhase::Failed,
            format!(
                "Compaction failed: {}",
                row.error().get().message().get().as_str()
            ),
        ),
    };
    // These are the public record's own facts, including any supplied token
    // totals. Missing totals stay absent; provider call usage is not substituted.
    let value = serde_json::to_value(row).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    if phase != CompactionPhase::Running {
        if let Some(reason) = value.get("reason").and_then(serde_json::Value::as_str) {
            text.push_str(&format!("\n\n**Reason:** {reason}"));
        }
        if let Some(recent) = value.get("recent") {
            let shown = if let Some(text) = recent.as_str() {
                text.to_owned()
            } else {
                serde_json::to_string_pretty(recent).map_err(|error| WireError::Codec {
                    reason: error.to_string(),
                })?
            };
            text.push_str(&format!("\n\n### Kept\n\n{shown}"));
        }
        if let Some(metadata) = value.get("metadata").filter(|v| !v.is_null()) {
            let shown =
                serde_json::to_string_pretty(metadata).map_err(|error| WireError::Codec {
                    reason: error.to_string(),
                })?;
            text.push_str(&format!("\n\n### Metadata\n\n```json\n{shown}\n```"));
        }
    }
    Ok(DurableMessage::Compaction {
        info: Compaction { id, phase },
        text,
    })
}
