//! Cohere token counters, separate from monetary billed units.
// Copyright 2023 Vercel, Inc. SPDX-License-Identifier: Apache-2.0
// Adapted from vercel/ai packages/cohere/src/convert-cohere-usage.ts at
// 7772d0ca0f7abc2cb2a0ad61b0272a72655d780d. Modified for exact provenance.
use super::*;

pub(crate) fn normalize_cohere_usage(
    value: &Value,
) -> Result<Option<TokenUsage>, UsageNormalizationError> {
    let route = UsageRoute::CohereChat;
    let Some(usage) = optional_object(value, "usage", route, UsageWireField::Usage)? else {
        return Ok(None);
    };
    let tokens = optional_object_from_map(usage, "tokens", route, UsageWireField::InputDetails)?;
    let input =
        optional_nested_reported(tokens, "input_tokens", route, UsageWireField::InputTotal)?;
    let output = optional_nested_reported(tokens, "output_tokens", route, UsageWireField::Output)?;
    let cache = optional_reported(usage, "cached_tokens", route, UsageWireField::CacheRead)?;
    // Cohere's usage object reports input/output token counts and an optional
    // top-level cached_tokens; the protocol has no cache-write coordinate and
    // reports the read counter only when the cache contributed, so an omitted
    // counter is a zero contribution. The zero enters the derivation only:
    // the stored read leaf stays absent when the wire omitted it.
    let cache_read_for_derivation = match cache.count() {
        Some(count) => UsageField::reported(count),
        None => UsageField::reported(TokenCount::ZERO),
    };
    let fresh = derive_fresh(
        &input,
        &cache_read_for_derivation,
        &UsageField::reported(TokenCount::ZERO),
        route,
    )?;
    Ok(Some(
        TokenUsage::new(
            input,
            fresh,
            output,
            UsageField::ABSENT,
            cache,
            UsageField::ABSENT,
            UsageField::ABSENT,
        )
        .with_protocol(UsageProtocolFamily::InclusiveInputNoCache),
    ))
}
