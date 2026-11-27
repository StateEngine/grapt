//! A result reference captured for one permitted provider attempt.
use grapt_types::{EventSeq, MessageId, PhysicalAttemptId, SessionId, StepNumber};
use serde_json::{json, Value};

pub const MAX_PRESENTATION_BYTES: usize = 12_288;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationMailPresented {
    pub session: SessionId,
    pub input: MessageId,
    pub source_seq: EventSeq,
    pub attempt: PhysicalAttemptId,
    pub step: StepNumber,
}

impl OperationMailPresented {
    pub fn encode(&self) -> Value {
        json!({"sessionID":self.session.as_str(), "inputID":self.input.as_str(),
            "sourceSeq":self.source_seq.value(), "attemptID":self.attempt.as_str(), "step":self.step.value()})
    }

    pub fn decode(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or("operation mail capture must be an object")?;
        if object.len() != 5 {
            return Err("operation mail capture has unknown or missing fields".into());
        }
        let text = |key: &str| -> Result<&str, String> {
            let text = value
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("mail capture has no {key}"))?;
            if text.is_empty() || text.len() > 512 {
                return Err(format!("mail capture {key} exceeds its identity bound"));
            }
            Ok(text)
        };
        Ok(Self {
            session: text("sessionID")?
                .parse()
                .map_err(|error| format!("capture session: {error}"))?,
            input: text("inputID")?
                .parse()
                .map_err(|error| format!("capture input: {error}"))?,
            source_seq: EventSeq::new(
                value
                    .get("sourceSeq")
                    .and_then(Value::as_u64)
                    .ok_or("mail capture has no source sequence")?,
            ),
            attempt: PhysicalAttemptId::new(text("attemptID")?).ok_or("capture attempt")?,
            step: value
                .get("step")
                .and_then(Value::as_u64)
                .and_then(|step| u32::try_from(step).ok())
                .and_then(StepNumber::new)
                .ok_or("mail capture step is not positive")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_round_trip_and_invalid_coordinates_refuse() -> Result<(), String> {
        let raw = json!({"sessionID":"ses_parent", "inputID":"msg_result", "sourceSeq":7,
            "attemptID":"attempt_result", "step":1});
        let capture = OperationMailPresented::decode(&raw)?;
        assert_eq!(capture.encode(), raw);
        for (key, bad) in [
            ("step", json!(0)),
            ("sourceSeq", json!(-1)),
            ("inputID", json!("ses_foreign")),
            ("attemptID", json!("x".repeat(513))),
        ] {
            let mut malformed = raw.clone();
            malformed[key] = bad;
            assert!(OperationMailPresented::decode(&malformed).is_err());
        }
        let mut extra = raw;
        extra["projection"] = json!("caller-authored output");
        assert!(OperationMailPresented::decode(&extra).is_err());
        Ok(())
    }
}
