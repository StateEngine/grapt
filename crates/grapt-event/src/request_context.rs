//! Request/attempt accounting joins the same committed context-window identity.
use grapt_types::{
    ContextEpoch, ContextWindowId, EventId, PhysicalAttemptId, RequestContextIdentity, StepNumber,
};
use serde_json::{json, Value};

pub fn encode(
    context: &RequestContextIdentity,
    attempt: &PhysicalAttemptId,
    step: StepNumber,
) -> Value {
    json!({"window":context.window.as_str(),"epoch":context.epoch.value(),"descriptor":context.descriptor.as_str(),"attempt":attempt.as_str(),"step":step.value()})
}

pub fn decode(
    value: &Value,
) -> Result<(RequestContextIdentity, PhysicalAttemptId, StepNumber), String> {
    let object = value
        .as_object()
        .ok_or("request context is not an object")?;
    if object.len() != 5 {
        return Err("request context has unknown or missing fields".into());
    }
    let string = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty() && text.len() <= 256)
            .ok_or_else(|| format!("invalid request context {key}"))
    };
    Ok((
        RequestContextIdentity {
            window: ContextWindowId::new(string("window")?).ok_or("invalid window")?,
            epoch: ContextEpoch::new(
                object
                    .get("epoch")
                    .and_then(Value::as_u64)
                    .ok_or("invalid epoch")?,
            ),
            descriptor: string("descriptor")?
                .parse::<EventId>()
                .map_err(|_| "invalid descriptor")?,
        },
        PhysicalAttemptId::new(string("attempt")?).ok_or("invalid attempt")?,
        object
            .get("step")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .and_then(StepNumber::new)
            .ok_or("invalid Step")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scc_request_context_codec_preserves_window_accounting_join() -> Result<(), String> {
        let value = json!({"window":"window-scc","epoch":2,"descriptor":"evt_scc","attempt":"attempt-scc","step":3});
        let (context, attempt, step) = decode(&value)?;
        assert_eq!(encode(&context, &attempt, step), value);
        for key in ["window", "epoch", "descriptor", "attempt", "step"] {
            let mut invalid = value.clone();
            invalid[key] = Value::Null;
            assert!(decode(&invalid).is_err());
        }
        let mut invalid = value;
        invalid["extra"] = json!(true);
        assert!(decode(&invalid).is_err());
        Ok(())
    }
}
