use super::WireError;
use crate::cache::CacheUsage;
use grapt_api::u3a::TokenUsageObservationDto;

pub(super) fn observation(
    usage: Option<&TokenUsageObservationDto>,
) -> Result<CacheUsage, WireError> {
    let Some(usage) = usage else {
        return Ok(CacheUsage::default());
    };
    let invalid = || WireError::Invalid {
        what: "observed cache usage",
        value: "inconsistent token counts".to_owned(),
    };
    let value = serde_json::to_value(usage).map_err(|_| invalid())?;
    let count = |key| value.get(key).and_then(serde_json::Value::as_u64);
    let read = count("cacheRead");
    let partition = match (count("inputFresh"), read, count("cacheWrite")) {
        (Some(fresh), Some(read), Some(write)) => Some(
            fresh
                .checked_add(read)
                .and_then(|input| input.checked_add(write))
                .filter(|input| *input <= grapt_api::u3a::SafeTokenCount::MAX)
                .ok_or_else(invalid)?,
        ),
        _ => None,
    };
    let input = count("inputTotal").or(partition);
    if input.zip(read).is_some_and(|(input, read)| read > input)
        || count("inputTotal")
            .zip(partition)
            .is_some_and(|(input, partition)| input != partition)
    {
        return Err(invalid());
    }
    Ok(CacheUsage {
        estimated_usd: None,
        input,
        context: count("total")
            .or_else(|| {
                input
                    .zip(count("output"))
                    .and_then(|(a, b)| a.checked_add(b))
            })
            .or(input),
        read,
        request_change: None,
        observed_calls: None,
        usage_warning: None,
    })
}

impl super::DurableMessage {
    pub fn cache_usage(&self) -> Option<CacheUsage> {
        match self {
            Self::Assistant {
                cache,
                finish,
                error,
                ..
            } if cache.context.is_some()
                || cache.input.is_some()
                || cache.read.is_some()
                || error.is_some()
                || matches!(finish, Some(value) if *value != super::DurableFinish::Unknown) =>
            {
                Some(**cache)
            }
            _ => None,
        }
    }
}

pub(super) fn assistant(
    message: &grapt_api::u3a::SessionMessageAssistantDto,
) -> Result<CacheUsage, WireError> {
    let mut usage = observation(message.observed_usage().as_present())?;
    usage.estimated_usd = message.metadata().as_present().and_then(|metadata| {
        let metadata = serde_json::to_value(metadata).ok()?;
        let cost = metadata.get("estimatedCost")?;
        if cost.get("status")?.as_str()? != "estimated"
            || cost.get("scope")?.as_str()? != "assistant-message-api-equivalent"
            || cost.get("policy")?.as_str()?.is_empty()
        {
            return None;
        }
        cost.get("usd")?
            .as_f64()
            .filter(|amount| amount.is_finite() && *amount >= 0.0)
    });
    usage.usage_warning = message.metadata().as_present().and_then(|metadata| {
        let warning = metadata.get("usageWarning")?;
        grapt_types::failure_diagnostic::UsageDiagnostic::from_words(
            warning.entry("field")?.as_str()?,
            warning.entry("issue")?.as_str()?,
        )
    });
    usage.request_change = message.metadata().as_present().and_then(request_change);
    usage.observed_calls = message.metadata().as_present().and_then(observed_calls);
    Ok(usage)
}
fn request_change(metadata: &grapt_api::u3a::OpenObject) -> Option<crate::cache::RequestChange> {
    use crate::cache::RequestChange;
    let index = || {
        metadata
            .get("cacheRequestChangedItem")?
            .as_str()?
            .parse::<u16>()
            .ok()
            .filter(|index| *index <= 512)
    };
    Some(match metadata.get("cacheRequestChange")?.as_str()? {
        "first_observation" => RequestChange::FirstObservation,
        "target_changed" => RequestChange::Target,
        "cache_key_changed" => RequestChange::CacheKey,
        "routing_changed" => RequestChange::Routing,
        "tools_changed" => RequestChange::Tools,
        "instructions_changed" => RequestChange::Instructions,
        "options_changed" => RequestChange::Options,
        "input_changed" => RequestChange::Input { index: index()? },
        "input_shortened" => RequestChange::Shortened { index: index()? },
        "append_only" => RequestChange::AppendOnly,
        "identical" => RequestChange::Identical,
        "comparison_bound" => RequestChange::Bound,
        _ => return None,
    })
}

fn observed_calls(metadata: &grapt_api::u3a::OpenObject) -> Option<crate::cache::CallObservation> {
    let metadata = serde_json::to_value(metadata).ok()?;
    let calls = metadata.get("cacheObservedCalls")?;
    Some(match parse_calls(calls) {
        Some(calls) => crate::cache::CallObservation::Reported(calls),
        None => crate::cache::CallObservation::Unreadable,
    })
}
mod calls_cost;
fn parse_calls(calls: &serde_json::Value) -> Option<crate::cache::ObservedCalls> {
    let number = |key| {
        calls
            .get(key)?
            .as_u64()
            .filter(|value| *value <= grapt_api::u3a::SafeTokenCount::MAX)
    };
    let scope: [u8; 64] = calls.get("scope")?.as_str()?.as_bytes().try_into().ok()?;
    if !scope.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let attempts = number("attempts")?;
    let unreported = number("unreported")?;
    let input = number("pairedInput")?;
    let read = number("pairedRead")?;
    let writes = number("knownWrite")?;
    let unreported_writes = number("unreportedWrites")?;
    if unreported > attempts
        || read > input
        || unreported_writes > attempts
        || (attempts == 0 && (input != 0 || read != 0 || writes != 0))
    {
        return None;
    }
    Some(crate::cache::ObservedCalls {
        cost: calls_cost::parse(calls, attempts),
        scope,
        attempts,
        unreported,
        input,
        read,
        write: (unreported_writes == 0).then_some(writes),
        loss: number("lostAtLeast")?,
        overflow: calls.get("overflow")?.as_bool()?,
    })
}

#[cfg(test)]
mod call_tests {
    use super::*;
    #[test]
    fn missing_and_malformed_call_metadata_have_distinct_projections() -> Result<(), String> {
        let missing: grapt_api::u3a::OpenObject =
            serde_json::from_value(serde_json::json!({})).map_err(|error| error.to_string())?;
        assert_eq!(observed_calls(&missing), None);
        for calls in [
            serde_json::Value::Null,
            serde_json::json!({
                "scope":"a".repeat(64),"attempts":1,"unreported":0,"pairedInput":100,"pairedRead":101,
                "knownWrite":0,"unreportedWrites":0,"lostAtLeast":0,"overflow":false,
            }),
        ] {
            let metadata = serde_json::from_value(serde_json::json!({"cacheObservedCalls":calls}))
                .map_err(|error| error.to_string())?;
            assert_eq!(
                observed_calls(&metadata),
                Some(crate::cache::CallObservation::Unreadable)
            );
        }
        Ok(())
    }
}
