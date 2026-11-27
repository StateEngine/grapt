//! Google reports total output across visible candidate tokens and hidden thought tokens.
use super::*;

pub(crate) fn normalize(value: &Value) -> Result<Option<TokenUsage>, UsageNormalizationError> {
    let route = UsageRoute::GoogleGemini;
    let Some(usage) = optional_object(value, "usageMetadata", route, UsageWireField::Usage)? else {
        return Ok(None);
    };
    let input = optional_reported(usage, "promptTokenCount", route, UsageWireField::InputTotal)?;
    let tool_prompt = optional_reported(
        usage,
        "toolUsePromptTokenCount",
        route,
        UsageWireField::InputTotal,
    )?;
    let input = match (input.count(), tool_prompt.count()) {
        (Some(prompt), Some(tools)) => UsageField::derived(
            prompt.checked_add(tools).map_err(|e| {
                error(
                    route,
                    UsageWireField::InputTotal,
                    UsageNormalizationReason::OutOfRange(e),
                )
            })?,
            UsageDerivation::InputFromPromptAndToolUse,
        ),
        (_, None) => input,
        (None, Some(_)) => UsageField::ABSENT,
    };
    let cached = optional_reported(
        usage,
        "cachedContentTokenCount",
        route,
        UsageWireField::CacheRead,
    )?;
    let thoughts = optional_reported(
        usage,
        "thoughtsTokenCount",
        route,
        UsageWireField::Reasoning,
    )?;
    let candidate =
        optional_reported(usage, "candidatesTokenCount", route, UsageWireField::Output)?;
    let total = optional_reported(usage, "totalTokenCount", route, UsageWireField::Total)?;
    let output = match (total.count(), input.count()) {
        (Some(total), Some(input)) => {
            let raw = total.value().checked_sub(input.value()).ok_or_else(|| {
                error(
                    route,
                    UsageWireField::Output,
                    UsageNormalizationReason::PartitionExceedsTotal,
                )
            })?;
            let count = TokenCount::new(raw).map_err(|e| {
                error(
                    route,
                    UsageWireField::Output,
                    UsageNormalizationReason::OutOfRange(e),
                )
            })?;
            if candidate.value().is_some_and(|value| value > raw)
                || thoughts.value().is_some_and(|value| value > raw)
                || candidate
                    .value()
                    .zip(thoughts.value())
                    .is_some_and(|(a, b)| a.checked_add(b) != Some(raw))
            {
                return Err(error(
                    route,
                    UsageWireField::Output,
                    UsageNormalizationReason::ReportedTotalMismatch,
                ));
            }
            UsageField::derived(count, UsageDerivation::OutputFromTotalLessInput)
        }
        _ => UsageField::ABSENT,
    };
    let fresh = derive_fresh(
        &input,
        &cached,
        &UsageField::reported(TokenCount::ZERO),
        route,
    )?;
    // Google's usageMetadata reports promptTokenCount INCLUSIVE of cache;
    // cachedContentTokenCount is optional, so its omission does not prove a
    // zero cache, and the wire has no cache-write coordinate.
    Ok(Some(
        TokenUsage::new(
            input,
            fresh,
            output,
            thoughts,
            cached,
            UsageField::ABSENT,
            total,
        )
        .with_protocol(UsageProtocolFamily::InclusiveInput),
    ))
}
