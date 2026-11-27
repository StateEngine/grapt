//! Current and legacy usage payload codecs, including field provenance.
use super::{
    invalid, object, reject_unknown, required_str, required_u64, PayloadError, OBSERVED_V1_USAGE,
    OBSERVED_V2_FIELD, OBSERVED_V2_USAGE,
};
use grapt_types::usage::UsageDerivation;
use grapt_types::{
    AttemptUsage, LegacyAttemptUsage, PolicyVersion, TokenCount, UsageField, UsageObservationPhase,
    UsageProtocolFamily, UsageProvenance,
};
use serde_json::{Map, Value};

/// Usage encoding OMITS an absent field rather than writing a zero or a null.
///
/// This is the whole point of the row: "Absent is not zero, reported is not
/// estimated." A zero would be charged; a null would decode as an observed
/// nothing under a codec that maps null to a value. Omission is the only
/// encoding that survives the round trip as [`UsageField::ABSENT`].
pub(super) fn encode_usage(usage: &AttemptUsage) -> Value {
    let mut out = Map::new();
    encode_field(&mut out, "inputTotalTokens", usage.input_total_tokens());
    encode_field(&mut out, "inputFreshTokens", usage.input_tokens());
    encode_field(&mut out, "outputTokens", usage.output_tokens());
    encode_field(&mut out, "reasoningTokens", usage.reasoning_tokens());
    encode_field(
        &mut out,
        "cacheReadInputTokens",
        usage.cache_read_input_tokens(),
    );
    encode_field(
        &mut out,
        "cacheWriteInputTokens",
        usage.cache_write_input_tokens(),
    );
    encode_field(&mut out, "totalTokens", usage.total_tokens());
    if let Some(family) = usage.protocol() {
        out.insert("protocol".to_owned(), Value::from(protocol_tag(family)));
    }
    out.insert("phase".to_owned(), Value::from(phase_tag(usage.phase())));
    Value::Object(out)
}

fn encode_field(out: &mut Map<String, Value>, name: &str, field: &UsageField) {
    let Some(measurement) = field.measurement() else {
        return;
    };
    let mut encoded = Map::new();
    encoded.insert("count".to_owned(), Value::from(measurement.count().value()));
    match measurement.provenance() {
        UsageProvenance::ProviderReported => {
            encoded.insert("provenance".to_owned(), Value::from("provider-reported"));
        }
        UsageProvenance::ProviderDerived { rule } => {
            encoded.insert("provenance".to_owned(), Value::from("provider-derived"));
            encoded.insert("rule".to_owned(), Value::from(derivation_tag(*rule)));
        }
        UsageProvenance::HarnessEstimated { policy } => {
            encoded.insert("provenance".to_owned(), Value::from("harness-estimated"));
            encoded.insert("policy".to_owned(), Value::from(policy.as_str()));
        }
    }
    out.insert(name.to_owned(), Value::Object(encoded));
}

pub(super) fn decode_usage_v2(value: &Value) -> Result<AttemptUsage, PayloadError> {
    let out = object(value, OBSERVED_V2_USAGE)?;
    let input_total_tokens = optional_field_v2(out, "inputTotalTokens")?;
    let input_tokens = optional_field_v2(out, "inputFreshTokens")?;
    let output_tokens = optional_field_v2(out, "outputTokens")?;
    let reasoning_tokens = optional_field_v2(out, "reasoningTokens")?;
    let cache_read_input_tokens = optional_field_v2(out, "cacheReadInputTokens")?;
    let cache_write_input_tokens = optional_field_v2(out, "cacheWriteInputTokens")?;
    let total_tokens = optional_field_v2(out, "totalTokens")?;
    let phase = phase_of(
        required_str(out, OBSERVED_V2_USAGE, "phase")?,
        OBSERVED_V2_USAGE,
    )?;
    let protocol = protocol_of(out.get("protocol").and_then(Value::as_str));
    reject_unknown(
        out,
        OBSERVED_V2_USAGE,
        &[
            "inputTotalTokens",
            "inputFreshTokens",
            "outputTokens",
            "reasoningTokens",
            "cacheReadInputTokens",
            "cacheWriteInputTokens",
            "totalTokens",
            "phase",
            "protocol",
        ],
    )?;
    Ok(match protocol {
        Some(family) => AttemptUsage::observed_stamped(
            input_total_tokens,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_input_tokens,
            cache_write_input_tokens,
            total_tokens,
            phase,
            family,
        ),
        None => AttemptUsage::observed(
            input_total_tokens,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_input_tokens,
            cache_write_input_tokens,
            total_tokens,
            phase,
        ),
    })
}

pub(super) fn decode_usage_v1(value: &Value) -> Result<LegacyAttemptUsage, PayloadError> {
    let out = object(value, OBSERVED_V1_USAGE)?;
    let input = optional_count_v1(out, "inputTokens")?;
    let output = optional_count_v1(out, "outputTokens")?;
    let reasoning = optional_count_v1(out, "reasoningTokens")?;
    let cache_read = optional_count_v1(out, "cacheReadInputTokens")?;
    let cache_write = optional_count_v1(out, "cacheWriteInputTokens")?;
    let phase = phase_of(
        required_str(out, OBSERVED_V1_USAGE, "phase")?,
        OBSERVED_V1_USAGE,
    )?;
    let provenance = match required_str(out, OBSERVED_V1_USAGE, "provenance")? {
        "provider-reported" => {
            reject_unknown(
                out,
                OBSERVED_V1_USAGE,
                &[
                    "inputTokens",
                    "outputTokens",
                    "reasoningTokens",
                    "cacheReadInputTokens",
                    "cacheWriteInputTokens",
                    "phase",
                    "provenance",
                ],
            )?;
            UsageProvenance::ProviderReported
        }
        "harness-estimated" => {
            let policy = PolicyVersion::new(required_str(out, OBSERVED_V1_USAGE, "policy")?)
                .ok_or_else(|| invalid(OBSERVED_V1_USAGE, "policy", "a non-empty PolicyVersion"))?;
            reject_unknown(
                out,
                OBSERVED_V1_USAGE,
                &[
                    "inputTokens",
                    "outputTokens",
                    "reasoningTokens",
                    "cacheReadInputTokens",
                    "cacheWriteInputTokens",
                    "phase",
                    "provenance",
                    "policy",
                ],
            )?;
            UsageProvenance::HarnessEstimated { policy }
        }
        other => {
            return Err(PayloadError::UnknownVariant {
                at: OBSERVED_V1_USAGE,
                field: "provenance".to_owned(),
                value: other.to_owned(),
            })
        }
    };
    Ok(LegacyAttemptUsage::from_v1_migration(
        legacy_field(input, &provenance),
        legacy_field(output, &provenance),
        legacy_field(reasoning, &provenance),
        legacy_field(cache_read, &provenance),
        legacy_field(cache_write, &provenance),
        phase,
    ))
}

fn protocol_tag(family: UsageProtocolFamily) -> &'static str {
    match family {
        UsageProtocolFamily::FreshInputPartition => "fresh-input-partition",
        UsageProtocolFamily::InclusiveInputNoCache => "inclusive-input-no-cache",
        UsageProtocolFamily::InclusiveInput => "inclusive-input",
    }
}

/// A missing, null, non-string, or unknown protocol tag decodes as
/// UNSTAMPED. Unlike a value field, the stamp degrades instead of refusing:
/// it is advisory to accounting, and refusing the whole row over it would
/// make a readable history row unreadable. The leaves and their provenance
/// remain the durable truth.
fn protocol_of(raw: Option<&str>) -> Option<UsageProtocolFamily> {
    match raw {
        Some("fresh-input-partition") => Some(UsageProtocolFamily::FreshInputPartition),
        Some("inclusive-input-no-cache") => Some(UsageProtocolFamily::InclusiveInputNoCache),
        Some("inclusive-input") => Some(UsageProtocolFamily::InclusiveInput),
        _ => None,
    }
}

fn phase_tag(phase: UsageObservationPhase) -> &'static str {
    match phase {
        UsageObservationPhase::DuringStream => "during-stream",
        UsageObservationPhase::Terminal => "terminal",
        UsageObservationPhase::Reconciled => "reconciled",
    }
}

fn phase_of(raw: &str, at: &'static str) -> Result<UsageObservationPhase, PayloadError> {
    match raw {
        "during-stream" => Ok(UsageObservationPhase::DuringStream),
        "terminal" => Ok(UsageObservationPhase::Terminal),
        "reconciled" => Ok(UsageObservationPhase::Reconciled),
        other => Err(PayloadError::UnknownVariant {
            at,
            field: "phase".to_owned(),
            value: other.to_owned(),
        }),
    }
}

/// An omitted usage field is [`UsageField::ABSENT`]; an explicitly null one is
/// a typed refusal.
///
/// The distinction is the codec ledger's `Optional<T>` category exactly:
/// missing maps to absent, null REJECTS. Accepting null here would give a
/// writer two spellings for "not observed", and the two would reconcile
/// differently the moment one of them was mistaken for an observed value.
fn optional_field_v2(object: &Map<String, Value>, field: &str) -> Result<UsageField, PayloadError> {
    match object.get(field) {
        None => Ok(UsageField::ABSENT),
        Some(Value::Null) => Err(PayloadError::NullField {
            at: OBSERVED_V2_USAGE,
            field: field.to_owned(),
        }),
        Some(value) => decode_field_v2(value, field),
    }
}

fn decode_field_v2(value: &Value, name: &str) -> Result<UsageField, PayloadError> {
    let field = object(value, OBSERVED_V2_FIELD)?;
    let raw = required_u64(field, OBSERVED_V2_FIELD, "count")?;
    let count = TokenCount::new(raw)
        .map_err(|_| invalid(OBSERVED_V2_FIELD, "count", "a bounded exact token count"))?;
    let provenance = required_str(field, OBSERVED_V2_FIELD, "provenance")?;
    let observed = match provenance {
        "provider-reported" => {
            reject_unknown(field, OBSERVED_V2_FIELD, &["count", "provenance"])?;
            UsageField::reported(count)
        }
        "provider-derived" => {
            let rule = derivation_of(required_str(field, OBSERVED_V2_FIELD, "rule")?)?;
            reject_unknown(field, OBSERVED_V2_FIELD, &["count", "provenance", "rule"])?;
            UsageField::derived(count, rule)
        }
        "harness-estimated" => {
            let policy = PolicyVersion::new(required_str(field, OBSERVED_V2_FIELD, "policy")?)
                .ok_or_else(|| invalid(OBSERVED_V2_FIELD, "policy", "a non-empty PolicyVersion"))?;
            reject_unknown(field, OBSERVED_V2_FIELD, &["count", "provenance", "policy"])?;
            UsageField::estimated(count, policy)
        }
        other => {
            return Err(PayloadError::UnknownVariant {
                at: OBSERVED_V2_FIELD,
                field: format!("{name}.provenance"),
                value: other.to_owned(),
            })
        }
    };
    Ok(observed)
}

fn optional_count_v1(
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<TokenCount>, PayloadError> {
    match object.get(field) {
        None => Ok(None),
        Some(Value::Null) => Err(PayloadError::NullField {
            at: OBSERVED_V1_USAGE,
            field: field.to_owned(),
        }),
        Some(value) => {
            let raw = value
                .as_u64()
                .ok_or_else(|| invalid(OBSERVED_V1_USAGE, field, "a non-negative integer"))?;
            TokenCount::new(raw)
                .map(Some)
                .map_err(|_| invalid(OBSERVED_V1_USAGE, field, "a bounded exact token count"))
        }
    }
}

fn legacy_field(count: Option<TokenCount>, provenance: &UsageProvenance) -> UsageField {
    let Some(count) = count else {
        return UsageField::ABSENT;
    };
    match provenance {
        UsageProvenance::ProviderReported => UsageField::reported(count),
        UsageProvenance::ProviderDerived { rule } => UsageField::derived(count, *rule),
        UsageProvenance::HarnessEstimated { policy } => {
            UsageField::estimated(count, policy.clone())
        }
    }
}

fn derivation_tag(rule: UsageDerivation) -> &'static str {
    match rule {
        UsageDerivation::InputFromPromptAndToolUse => "input-from-prompt-and-tool-use",
        UsageDerivation::OutputFromTotalLessInput => "output-from-total-less-input",
        UsageDerivation::FreshFromInclusiveInput => "fresh-from-inclusive-input",
        UsageDerivation::InclusiveInputFromPartition => "inclusive-input-from-partition",
    }
}

fn derivation_of(raw: &str) -> Result<UsageDerivation, PayloadError> {
    match raw {
        "input-from-prompt-and-tool-use" => Ok(UsageDerivation::InputFromPromptAndToolUse),
        "output-from-total-less-input" => Ok(UsageDerivation::OutputFromTotalLessInput),
        "fresh-from-inclusive-input" => Ok(UsageDerivation::FreshFromInclusiveInput),
        "inclusive-input-from-partition" => Ok(UsageDerivation::InclusiveInputFromPartition),
        other => Err(PayloadError::UnknownVariant {
            at: OBSERVED_V2_FIELD,
            field: "rule".to_owned(),
            value: other.to_owned(),
        }),
    }
}
