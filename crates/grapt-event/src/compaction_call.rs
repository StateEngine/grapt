//! Additive v1 facts for the provider operation; the successful context
//! boundary remains the separate atomic `session.compaction.ended.2` descriptor.

use grapt_types::{CompactionId, PhysicalAttemptId, StepNumber};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallEnd {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallCoordinates {
    pub compaction: CompactionId,
    pub attempt: PhysicalAttemptId,
    pub step: StepNumber,
}

pub fn encode_started(coordinates: &CallCoordinates) -> Value {
    json!({"compaction":coordinates.compaction.as_str(),"attempt":coordinates.attempt.as_str(),"step":coordinates.step.value()})
}

pub fn decode_started(value: &Value) -> Result<CallCoordinates, String> {
    let object = value
        .as_object()
        .ok_or("compaction call is not an object")?;
    if object.len() != 3 {
        return Err("compaction call has unknown or missing fields".to_owned());
    }
    Ok(CallCoordinates {
        compaction: CompactionId::new(
            object
                .get("compaction")
                .and_then(Value::as_str)
                .ok_or("compaction id is absent")?,
        )
        .ok_or("compaction id is invalid")?,
        attempt: PhysicalAttemptId::new(
            object
                .get("attempt")
                .and_then(Value::as_str)
                .ok_or("attempt is absent")?,
        )
        .ok_or("attempt is invalid")?,
        step: object
            .get("step")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .and_then(StepNumber::new)
            .ok_or("step is invalid")?,
    })
}

pub fn encode_ended(coordinates: &CallCoordinates, outcome: CallEnd) -> Value {
    json!({"call":encode_started(coordinates),"outcome":match outcome { CallEnd::Completed => "completed", CallEnd::Failed => "failed", CallEnd::Cancelled => "cancelled" }})
}

pub fn decode_ended(value: &Value) -> Result<(CallCoordinates, CallEnd), String> {
    let object = value
        .as_object()
        .ok_or("compaction call end is not an object")?;
    if object.len() != 2 {
        return Err("compaction call end has unknown or missing fields".to_owned());
    }
    let coordinates = decode_started(object.get("call").ok_or("call coordinates absent")?)?;
    let outcome = match object.get("outcome").and_then(Value::as_str) {
        Some("completed") => CallEnd::Completed,
        Some("failed") => CallEnd::Failed,
        Some("cancelled") => CallEnd::Cancelled,
        _ => return Err("compaction call outcome is invalid".to_owned()),
    };
    Ok((coordinates, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scc_call_codec_refuses_unknown_null_and_invalid_coordinates() -> Result<(), String> {
        let value = json!({"compaction":"scc-call","attempt":"scc-attempt","step":1});
        let coordinates = decode_started(&value)?;
        assert_eq!(encode_started(&coordinates), value);
        for field in ["compaction", "attempt", "step"] {
            let mut corrupt = value.clone();
            corrupt[field] = Value::Null;
            assert!(decode_started(&corrupt).is_err());
        }
        let mut corrupt = value.clone();
        corrupt["extra"] = json!(true);
        assert!(decode_started(&corrupt).is_err());
        corrupt = value;
        corrupt["step"] = json!(0);
        assert!(decode_started(&corrupt).is_err());
        for outcome in [CallEnd::Completed, CallEnd::Failed, CallEnd::Cancelled] {
            assert_eq!(
                decode_ended(&encode_ended(&coordinates, outcome))?,
                (coordinates.clone(), outcome)
            );
        }
        assert!(
            decode_ended(&json!({"call":encode_started(&coordinates),"outcome":"unknown"}))
                .is_err()
        );
        Ok(())
    }
}
