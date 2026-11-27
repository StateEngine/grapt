//! Completed-reply presentation data from the existing public assistant DTO.
use super::*;

/// The completed assistant message's usage. These fields do not claim to be
/// all-call execution accounting (retries and compaction have separate public
/// projections). Timing may cover earlier tool Steps when their input boundary
/// is present in this bounded history fold.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantFooter {
    pub message: MessageWireId,
    pub model: grapt_api::u3a::ModelRefDto,
    pub created_ms: Option<u64>,
    pub completed_ms: Option<u64>,
    /// Earliest assistant start after an input boundary observed in this fold.
    /// Missing history never borrows an earlier execution's start.
    pub execution_started_ms: Option<u64>,
    /// Terminal classified by the history projection, never inferred from usage.
    pub outcome: Option<crate::contract::Outcome>,
    pub input: Option<u64>,
    pub output: Option<u64>,
    /// Read tokens paired with an observed inclusive input denominator.
    /// A legacy input count alone cannot establish a cache read share.
    pub cache_read: Option<u64>,
    pub estimated_usd: Option<f64>,
    pub usage_warning: Option<grapt_types::failure_diagnostic::UsageDiagnostic>,
}

pub(super) fn decode(
    message: &grapt_api::u3a::SessionMessageAssistantDto,
    usage: Option<(u64, u64)>,
    cache: crate::cache::CacheUsage,
) -> AssistantFooter {
    let output = message
        .observed_usage()
        .as_present()
        .and_then(|usage| usage.output().as_present())
        .and_then(|value| serde_json::to_value(value).ok())
        .and_then(|value| value.as_u64())
        .or(usage.map(|(_, output)| output));
    AssistantFooter {
        message: message.id().get().clone(),
        model: message.model().get().clone(),
        created_ms: timestamp(message.time().get().created().get()),
        completed_ms: message
            .time()
            .get()
            .completed()
            .as_present()
            .and_then(timestamp),
        execution_started_ms: None,
        outcome: None,
        input: cache.input.or(usage.map(|(input, _)| input)),
        output,
        cache_read: cache.read.filter(|_| cache.input.is_some()),
        estimated_usd: cache.estimated_usd,
        usage_warning: cache.usage_warning,
    }
}

fn timestamp(value: &FiniteF64) -> Option<u64> {
    let number = serde_json::to_value(value).ok()?.as_f64()?;
    (number >= 0.0 && number.fract() == 0.0 && number <= grapt_api::u3a::SafeTokenCount::MAX as f64)
        .then_some(number as u64)
}
