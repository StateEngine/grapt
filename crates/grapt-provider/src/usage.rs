//! Bounded provider-usage normalization.
//!
//! Wire routes disagree about whether their input counter is fresh or
//! inclusive. They normalize here into one model-agnostic shape before a
//! fragment can cross into the runtime. Every present wire leaf is validated
//! as a bounded [`TokenCount`]; every derived leaf names its exact equation.

use grapt_types::usage::{UsageDerivation, UsageField, UsageProtocolFamily};
use grapt_types::{TokenCount, TokenCountError};
use serde_json::{Map, Value};
mod cohere;
mod gemini;
pub(crate) use cohere::normalize_cohere_usage;
pub(crate) use gemini::normalize as normalize_gemini_usage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageRoute {
    GoogleGemini,
    CohereChat,
    OpenAiResponses,
    ChatGptCodex,
    AnthropicMessages,
    OpenAiCompatible,
    BedrockConverse,
}

impl UsageRoute {
    fn label(self) -> &'static str {
        match self {
            Self::GoogleGemini => "google-gemini",
            Self::CohereChat => "cohere-chat",
            Self::OpenAiResponses => "openai-responses",
            Self::ChatGptCodex => "chatgpt-codex",
            Self::AnthropicMessages => "anthropic-messages",
            Self::OpenAiCompatible => "openai-compatible",
            Self::BedrockConverse => "bedrock-converse",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageWireField {
    Usage,
    InputTotal,
    InputFresh,
    CacheRead,
    CacheWrite,
    Output,
    Reasoning,
    Total,
    InputDetails,
    OutputDetails,
}

impl UsageWireField {
    fn label(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::InputTotal => "input-total",
            Self::InputFresh => "input-fresh",
            Self::CacheRead => "cache-read-input",
            Self::CacheWrite => "cache-write-input",
            Self::Output => "output",
            Self::Reasoning => "reasoning",
            Self::Total => "total",
            Self::InputDetails => "input-details",
            Self::OutputDetails => "output-details",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageNormalizationReason {
    Null,
    WrongType,
    OutOfRange(TokenCountError),
    PartitionExceedsTotal,
    ReportedTotalMismatch,
    ConflictingFrames,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageNormalizationError {
    route: UsageRoute,
    field: UsageWireField,
    reason: UsageNormalizationReason,
}

impl UsageNormalizationError {
    pub fn route(&self) -> UsageRoute {
        self.route
    }

    pub fn field(&self) -> UsageWireField {
        self.field
    }

    /// Safe structured diagnosis, before the explanatory string is built.
    pub fn diagnostic(&self) -> grapt_types::failure_diagnostic::UsageDiagnostic {
        use grapt_types::failure_diagnostic::{UsageDiagnostic, UsageIssue};
        use grapt_types::usage::UsageFieldKind as Field;
        let field = match self.field {
            UsageWireField::InputTotal => Some(Field::InputTotal),
            UsageWireField::InputFresh => Some(Field::InputFresh),
            UsageWireField::CacheRead => Some(Field::CacheRead),
            UsageWireField::CacheWrite => Some(Field::CacheWrite),
            UsageWireField::Output => Some(Field::Output),
            UsageWireField::Reasoning => Some(Field::Reasoning),
            UsageWireField::Total => Some(Field::Total),
            UsageWireField::Usage
            | UsageWireField::InputDetails
            | UsageWireField::OutputDetails => None,
        };
        let issue = match self.reason {
            UsageNormalizationReason::Null => UsageIssue::Null,
            UsageNormalizationReason::WrongType => UsageIssue::WrongType,
            UsageNormalizationReason::OutOfRange(_) => UsageIssue::OutOfRange,
            UsageNormalizationReason::PartitionExceedsTotal => UsageIssue::PartitionExceedsTotal,
            UsageNormalizationReason::ReportedTotalMismatch => UsageIssue::TotalMismatch,
            UsageNormalizationReason::ConflictingFrames => UsageIssue::ConflictingSnapshots,
        };
        UsageDiagnostic { field, issue }
    }

    pub fn reason(&self) -> &UsageNormalizationReason {
        &self.reason
    }
}

impl core::fmt::Display for UsageNormalizationError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{} usage field {} is invalid: ",
            self.route.label(),
            self.field.label()
        )?;
        match &self.reason {
            UsageNormalizationReason::Null => f.write_str("explicit null is not absence"),
            UsageNormalizationReason::WrongType => {
                f.write_str("expected a non-negative integral token count")
            }
            UsageNormalizationReason::OutOfRange(error) => error.fmt(f),
            UsageNormalizationReason::PartitionExceedsTotal => {
                f.write_str("cache partition exceeds inclusive input")
            }
            UsageNormalizationReason::ReportedTotalMismatch => {
                f.write_str("reported total does not equal input plus output")
            }
            UsageNormalizationReason::ConflictingFrames => {
                f.write_str("two frames report different values for one leaf")
            }
        }
    }
}

impl std::error::Error for UsageNormalizationError {}

/// Normalized usage carried by a provider fragment. The fields are private so
/// a route cannot accidentally swap fresh and inclusive input by struct-literal
/// position.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenUsage {
    input_total: UsageField,
    input_fresh: UsageField,
    output: UsageField,
    reasoning: UsageField,
    cache_read: UsageField,
    cache_write: UsageField,
    total: UsageField,
    protocol: Option<UsageProtocolFamily>,
    protocol_conflict: bool,
}

impl TokenUsage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input_total: UsageField,
        input_fresh: UsageField,
        output: UsageField,
        reasoning: UsageField,
        cache_read: UsageField,
        cache_write: UsageField,
        total: UsageField,
    ) -> Self {
        Self {
            input_total,
            input_fresh,
            output,
            reasoning,
            cache_read,
            cache_write,
            total,
            protocol: None,
            protocol_conflict: false,
        }
    }

    pub fn input_total(&self) -> &UsageField {
        &self.input_total
    }

    pub fn input_fresh(&self) -> &UsageField {
        &self.input_fresh
    }

    pub fn output(&self) -> &UsageField {
        &self.output
    }

    pub fn reasoning(&self) -> &UsageField {
        &self.reasoning
    }

    pub fn cache_read(&self) -> &UsageField {
        &self.cache_read
    }

    pub fn cache_write(&self) -> &UsageField {
        &self.cache_write
    }

    pub fn total(&self) -> &UsageField {
        &self.total
    }

    /// The protocol family this route's wire guarantees, stamped by the
    /// normalizer. Absent only for constructions no normalizer authored.
    pub fn protocol(&self) -> Option<UsageProtocolFamily> {
        self.protocol
    }

    /// A merge saw two different family stamps. Sticky: a later frame cannot
    /// clear it, and the route records it on the non-fatal usage warning
    /// path instead of letting it disappear.
    pub(crate) fn protocol_conflict(&self) -> bool {
        self.protocol_conflict
    }

    /// Stamp the protocol family the normalizer observed. The stamp rides
    /// along to the durable observation; it never rewrites a leaf.
    pub(crate) fn with_protocol(self, family: UsageProtocolFamily) -> Self {
        Self {
            protocol: Some(family),
            ..self
        }
    }

    /// Merge partial observations. Anthropic alone reports cumulative
    /// snapshots: later counts may increase, while decreases remain invalid.
    pub(crate) fn merge(
        &self,
        later: &Self,
        route: UsageRoute,
    ) -> Result<Self, UsageNormalizationError> {
        // A family conflict never errors: this merge sits on the turn path
        // and usage may not fail work. The stamp drops to unstamped and the
        // indication stays sticky — a later frame cannot restore the stamp
        // or erase the conflict; the route records the conflict on the
        // non-fatal usage warning channel, and the durable observation
        // carries the unstamped leaves so the reader falls back to the
        // recorded derivation inferences instead of a family it cannot
        // trust.
        let conflicted = self.protocol_conflict
            || later.protocol_conflict
            || matches!((self.protocol, later.protocol), (Some(first), Some(second)) if first != second);
        let protocol = if conflicted {
            None
        } else {
            self.protocol.or(later.protocol)
        };
        let mut merged = Self {
            input_total: merge_field(
                &self.input_total,
                &later.input_total,
                route,
                UsageWireField::InputTotal,
            )?,
            input_fresh: merge_field(
                &self.input_fresh,
                &later.input_fresh,
                route,
                UsageWireField::InputFresh,
            )?,
            output: merge_field(&self.output, &later.output, route, UsageWireField::Output)?,
            reasoning: merge_field(
                &self.reasoning,
                &later.reasoning,
                route,
                UsageWireField::Reasoning,
            )?,
            cache_read: merge_field(
                &self.cache_read,
                &later.cache_read,
                route,
                UsageWireField::CacheRead,
            )?,
            cache_write: merge_field(
                &self.cache_write,
                &later.cache_write,
                route,
                UsageWireField::CacheWrite,
            )?,
            total: merge_field(&self.total, &later.total, route, UsageWireField::Total)?,
            protocol,
            protocol_conflict: conflicted,
        };
        if route == UsageRoute::AnthropicMessages {
            // Partial cumulative snapshots can change one input partition
            // leaf without repeating the others. Derive from the merged
            // reported leaves, never retain an earlier derived total.
            merged.input_total = derive_inclusive(
                &merged.input_fresh,
                &merged.cache_read,
                &merged.cache_write,
                route,
            )?;
        }
        Ok(merged)
    }
}

pub(crate) fn normalize_responses_usage(
    value: &Value,
    route: UsageRoute,
) -> Result<Option<TokenUsage>, UsageNormalizationError> {
    let Some(usage) = optional_object(value, "usage", route, UsageWireField::Usage)? else {
        return Ok(None);
    };
    let input_total = optional_reported(usage, "input_tokens", route, UsageWireField::InputTotal)?;
    let input_details = optional_object_from_map(
        usage,
        "input_tokens_details",
        route,
        UsageWireField::InputDetails,
    )?;
    let cache_read = optional_nested_reported(
        input_details,
        "cached_tokens",
        route,
        UsageWireField::CacheRead,
    )?;
    let cache_write = optional_nested_reported(
        input_details,
        "cache_write_tokens",
        route,
        UsageWireField::CacheWrite,
    )?;
    let output = optional_reported(usage, "output_tokens", route, UsageWireField::Output)?;
    let output_details = optional_object_from_map(
        usage,
        "output_tokens_details",
        route,
        UsageWireField::OutputDetails,
    )?;
    let reasoning = optional_nested_reported(
        output_details,
        "reasoning_tokens",
        route,
        UsageWireField::Reasoning,
    )?;
    let total = optional_reported(usage, "total_tokens", route, UsageWireField::Total)?;
    let input_fresh = derive_fresh(&input_total, &cache_read, &cache_write, route)?;
    validate_total(&input_total, &output, &total, route)?;
    // OpenAI's Responses-family wire (shared by the codex route) reports
    // input_tokens INCLUSIVE of cache; input_tokens_details.cached_tokens is
    // optional, so an omitted cache counter does not prove a zero cache.
    Ok(Some(
        TokenUsage::new(
            input_total,
            input_fresh,
            output,
            reasoning,
            cache_read,
            cache_write,
            total,
        )
        .with_protocol(UsageProtocolFamily::InclusiveInput),
    ))
}

/// Bedrock reports fresh input separately from cache reads and writes.
pub(crate) fn normalize_bedrock_usage(
    value: &Value,
) -> Result<Option<TokenUsage>, UsageNormalizationError> {
    let route = UsageRoute::BedrockConverse;
    let Some(usage) = optional_object(value, "usage", route, UsageWireField::Usage)? else {
        return Ok(None);
    };
    let fresh = optional_reported(usage, "inputTokens", route, UsageWireField::InputFresh)?;
    let read = optional_reported(
        usage,
        "cacheReadInputTokens",
        route,
        UsageWireField::CacheRead,
    )?;
    let write = optional_reported(
        usage,
        "cacheWriteInputTokens",
        route,
        UsageWireField::CacheWrite,
    )?;
    let output = optional_reported(usage, "outputTokens", route, UsageWireField::Output)?;
    let total = optional_reported(usage, "totalTokens", route, UsageWireField::Total)?;
    // Bedrock omits optional cache counters when caching did not contribute.
    // Sum only present partitions; keep absent cache measurements absent.
    let inclusive = match fresh.count() {
        Some(fresh) => {
            let count = [read.count(), write.count()]
                .into_iter()
                .flatten()
                .try_fold(fresh, TokenCount::checked_add)
                .map_err(|source| {
                    error(
                        route,
                        UsageWireField::InputTotal,
                        UsageNormalizationReason::OutOfRange(source),
                    )
                })?;
            UsageField::derived(count, UsageDerivation::InclusiveInputFromPartition)
        }
        None => UsageField::ABSENT,
    };
    validate_total(&inclusive, &output, &total, route)?;
    // Bedrock Converse reports fresh inputTokens beside the cache partitions
    // and omits the optional cache counters when caching did not contribute,
    // so an omitted counter is a zero contribution.
    Ok(Some(
        TokenUsage::new(
            inclusive,
            fresh,
            output,
            UsageField::ABSENT,
            read,
            write,
            total,
        )
        .with_protocol(UsageProtocolFamily::FreshInputPartition),
    ))
}

pub(crate) fn normalize_anthropic_usage(
    value: &Value,
) -> Result<Option<TokenUsage>, UsageNormalizationError> {
    let route = UsageRoute::AnthropicMessages;
    let Some(usage) = optional_object(value, "usage", route, UsageWireField::Usage)? else {
        return Ok(None);
    };
    let input_fresh = optional_reported(usage, "input_tokens", route, UsageWireField::InputFresh)?;
    let cache_read = optional_reported(
        usage,
        "cache_read_input_tokens",
        route,
        UsageWireField::CacheRead,
    )?;
    let cache_write = optional_reported(
        usage,
        "cache_creation_input_tokens",
        route,
        UsageWireField::CacheWrite,
    )?;
    let output = optional_reported(usage, "output_tokens", route, UsageWireField::Output)?;
    let input_total = derive_inclusive(&input_fresh, &cache_read, &cache_write, route)?;
    // Anthropic's Messages usage reports input_tokens as FRESH input beside
    // cache_read_input_tokens and cache_creation_input_tokens; the wire
    // carries both counters with zero when caching did not contribute, so an
    // omitted counter is a zero contribution.
    Ok(Some(
        TokenUsage::new(
            input_total,
            input_fresh,
            output,
            UsageField::ABSENT,
            cache_read,
            cache_write,
            UsageField::ABSENT,
        )
        .with_protocol(UsageProtocolFamily::FreshInputPartition),
    ))
}

pub(crate) fn normalize_compatible_usage(
    value: &Value,
) -> Result<Option<TokenUsage>, UsageNormalizationError> {
    let route = UsageRoute::OpenAiCompatible;
    let Some(usage) = optional_object(value, "usage", route, UsageWireField::Usage)? else {
        return Ok(None);
    };
    let input_total = optional_reported(usage, "prompt_tokens", route, UsageWireField::InputTotal)?;
    let input_details = optional_object_from_map(
        usage,
        "prompt_tokens_details",
        route,
        UsageWireField::InputDetails,
    )?;
    let cache_read = optional_nested_reported(
        input_details,
        "cached_tokens",
        route,
        UsageWireField::CacheRead,
    )?;
    // OpenRouter's chat route (Anthropic models behind the gateway) reports
    // cache creation on the same details object as cache reads. A provider
    // omitting the field keeps cache write absent while permitting the route's
    // zero-write equation. An explicit null leaves that partition unknown.
    let cache_write = optional_nested_reported(
        input_details,
        "cache_write_tokens",
        route,
        UsageWireField::CacheWrite,
    )?;
    let output = optional_reported(usage, "completion_tokens", route, UsageWireField::Output)?;
    let output_details = optional_object_from_map(
        usage,
        "completion_tokens_details",
        route,
        UsageWireField::OutputDetails,
    )?;
    let reasoning = optional_nested_reported(
        output_details,
        "reasoning_tokens",
        route,
        UsageWireField::Reasoning,
    )?;
    let total = optional_reported(usage, "total_tokens", route, UsageWireField::Total)?;
    let fresh_write = if input_details
        .and_then(|details| details.get("cache_write_tokens"))
        .is_none()
    {
        UsageField::reported(TokenCount::ZERO)
    } else {
        cache_write.clone()
    };
    let input_fresh = derive_fresh(&input_total, &cache_read, &fresh_write, route)?;
    validate_total(&input_total, &output, &total, route)?;
    // OpenAI-compatible chat wires report prompt_tokens INCLUSIVE of cache;
    // the prompt_tokens_details breakdown is provider-specific and optional,
    // so an omitted cache counter does not prove a zero cache.
    Ok(Some(
        TokenUsage::new(
            input_total,
            input_fresh,
            output,
            reasoning,
            cache_read,
            cache_write,
            total,
        )
        .with_protocol(UsageProtocolFamily::InclusiveInput),
    ))
}

fn optional_object<'a>(
    value: &'a Value,
    key: &str,
    route: UsageRoute,
    field: UsageWireField,
) -> Result<Option<&'a Map<String, Value>>, UsageNormalizationError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => Ok(Some(object)),
        Some(_) => Err(error(route, field, UsageNormalizationReason::WrongType)),
    }
}

fn optional_object_from_map<'a>(
    value: &'a Map<String, Value>,
    key: &str,
    route: UsageRoute,
    field: UsageWireField,
) -> Result<Option<&'a Map<String, Value>>, UsageNormalizationError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(object)) => Ok(Some(object)),
        Some(_) => Err(error(route, field, UsageNormalizationReason::WrongType)),
    }
}

fn optional_nested_reported(
    object: Option<&Map<String, Value>>,
    key: &str,
    route: UsageRoute,
    field: UsageWireField,
) -> Result<UsageField, UsageNormalizationError> {
    match object {
        Some(object) => optional_reported(object, key, route, field),
        None => Ok(UsageField::ABSENT),
    }
}

fn optional_reported(
    object: &Map<String, Value>,
    key: &str,
    route: UsageRoute,
    field: UsageWireField,
) -> Result<UsageField, UsageNormalizationError> {
    let Some(value) = object.get(key) else {
        return Ok(UsageField::ABSENT);
    };
    if value.is_null() {
        return Ok(UsageField::ABSENT);
    }
    let raw = value
        .as_u64()
        .ok_or_else(|| error(route, field, UsageNormalizationReason::WrongType))?;
    let count = TokenCount::new(raw)
        .map_err(|source| error(route, field, UsageNormalizationReason::OutOfRange(source)))?;
    Ok(UsageField::reported(count))
}

fn derive_fresh(
    input_total: &UsageField,
    cache_read: &UsageField,
    cache_write: &UsageField,
    route: UsageRoute,
) -> Result<UsageField, UsageNormalizationError> {
    let (Some(total), Some(read), Some(write)) =
        (input_total.count(), cache_read.count(), cache_write.count())
    else {
        return Ok(UsageField::ABSENT);
    };
    let used = read.checked_add(write).map_err(|source| {
        error(
            route,
            UsageWireField::InputFresh,
            UsageNormalizationReason::OutOfRange(source),
        )
    })?;
    let raw = total.value().checked_sub(used.value()).ok_or_else(|| {
        error(
            route,
            UsageWireField::InputFresh,
            UsageNormalizationReason::PartitionExceedsTotal,
        )
    })?;
    let fresh = TokenCount::new(raw).map_err(|source| {
        error(
            route,
            UsageWireField::InputFresh,
            UsageNormalizationReason::OutOfRange(source),
        )
    })?;
    Ok(UsageField::derived(
        fresh,
        UsageDerivation::FreshFromInclusiveInput,
    ))
}

fn derive_inclusive(
    fresh: &UsageField,
    cache_read: &UsageField,
    cache_write: &UsageField,
    route: UsageRoute,
) -> Result<UsageField, UsageNormalizationError> {
    let (Some(fresh), Some(read), Some(write)) =
        (fresh.count(), cache_read.count(), cache_write.count())
    else {
        return Ok(UsageField::ABSENT);
    };
    let total = fresh
        .checked_add(read)
        .and_then(|value| value.checked_add(write))
        .map_err(|source| {
            error(
                route,
                UsageWireField::InputTotal,
                UsageNormalizationReason::OutOfRange(source),
            )
        })?;
    Ok(UsageField::derived(
        total,
        UsageDerivation::InclusiveInputFromPartition,
    ))
}

fn validate_total(
    input: &UsageField,
    output: &UsageField,
    total: &UsageField,
    route: UsageRoute,
) -> Result<(), UsageNormalizationError> {
    let (Some(input), Some(output), Some(total)) = (input.count(), output.count(), total.count())
    else {
        return Ok(());
    };
    let expected = input.checked_add(output).map_err(|source| {
        error(
            route,
            UsageWireField::Total,
            UsageNormalizationReason::OutOfRange(source),
        )
    })?;
    if expected != total {
        return Err(error(
            route,
            UsageWireField::Total,
            UsageNormalizationReason::ReportedTotalMismatch,
        ));
    }
    Ok(())
}

fn merge_field(
    earlier: &UsageField,
    later: &UsageField,
    route: UsageRoute,
    field: UsageWireField,
) -> Result<UsageField, UsageNormalizationError> {
    match (earlier.measurement(), later.measurement()) {
        (None, None) => Ok(UsageField::ABSENT),
        (Some(_), None) => Ok(earlier.clone()),
        (None, Some(_)) => Ok(later.clone()),
        (Some(first), Some(second))
            if first == second
                || (route == UsageRoute::AnthropicMessages && second.count() >= first.count()) =>
        {
            Ok(later.clone())
        }
        (Some(_), Some(_)) => Err(error(
            route,
            field,
            UsageNormalizationReason::ConflictingFrames,
        )),
    }
}

fn error(
    route: UsageRoute,
    field: UsageWireField,
    reason: UsageNormalizationReason,
) -> UsageNormalizationError {
    UsageNormalizationError {
        route,
        field,
        reason,
    }
}

#[cfg(test)]
mod normalization_tests;
