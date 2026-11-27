//! Version 3 Step terminals retain the reserved event coordinates and separately
//! carry when settlement was observed. Version 2 codecs and bytes stay unchanged.
use grapt_types::Timestamp;
use serde_json::Value;

use crate::{session_step_ended, session_step_failed};

/// A real, positive completion observation, never a reservation or an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompletionObservedAt(Timestamp);

impl CompletionObservedAt {
    pub fn new(at: Timestamp) -> Option<Self> {
        (at.as_millis() > 0).then_some(Self(at))
    }

    pub fn as_millis(self) -> i64 {
        self.0.as_millis()
    }
}

/// One terminal classification and the observation made for that logical Step.
/// Retries share the Step observation; no attempt-ledger order selects it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedStep<T> {
    fact: T,
    completed_at: CompletionObservedAt,
}

impl<T> ObservedStep<T> {
    pub fn new(fact: T, completed_at: CompletionObservedAt) -> Self {
        Self { fact, completed_at }
    }

    pub fn fact(&self) -> &T {
        &self.fact
    }

    pub fn completed_at(&self) -> CompletionObservedAt {
        self.completed_at
    }
}

#[derive(Debug)]
pub enum PayloadError {
    NotAnObject,
    InvalidObservation,
    Terminal(String),
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnObject => formatter.write_str("Step completion payload is not an object"),
            Self::InvalidObservation => {
                formatter.write_str("completedAt must be a present positive i64 timestamp")
            }
            Self::Terminal(reason) => write!(formatter, "Step terminal payload refused: {reason}"),
        }
    }
}

impl std::error::Error for PayloadError {}

pub fn encode_ended(fact: &ObservedStep<session_step_ended::StepEndedFact>) -> Value {
    encode(session_step_ended::encode(fact.fact()), fact.completed_at())
}

pub fn encode_failed(
    fact: &ObservedStep<session_step_failed::StepFailedFact>,
) -> Result<Value, session_step_failed::FactError> {
    // The failure codec is fallible because a decoded-only reason may not be
    // written back; the observation wrapper carries the same refusal.
    Ok(encode(
        session_step_failed::encode(fact.fact())?,
        fact.completed_at(),
    ))
}

fn encode(mut value: Value, at: CompletionObservedAt) -> Value {
    // Both owning codecs produce objects. The observation is the sole v3 field.
    value["completedAt"] = Value::from(at.as_millis());
    value
}

pub fn decode_ended(
    value: &Value,
) -> Result<ObservedStep<session_step_ended::StepEndedFact>, PayloadError> {
    let (terminal, at) = decode(value)?;
    let fact = session_step_ended::decode(&terminal)
        .map_err(|error| PayloadError::Terminal(error.to_string()))?;
    Ok(ObservedStep::new(fact, at))
}

pub fn decode_failed(
    value: &Value,
) -> Result<ObservedStep<session_step_failed::StepFailedFact>, PayloadError> {
    let (terminal, at) = decode(value)?;
    let fact = session_step_failed::decode(&terminal)
        .map_err(|error| PayloadError::Terminal(error.to_string()))?;
    Ok(ObservedStep::new(fact, at))
}

fn decode(value: &Value) -> Result<(Value, CompletionObservedAt), PayloadError> {
    let mut root = value.as_object().ok_or(PayloadError::NotAnObject)?.clone();
    let at = root
        .remove("completedAt")
        .and_then(|value| value.as_i64())
        .and_then(|value| CompletionObservedAt::new(Timestamp::from_millis(value)))
        .ok_or(PayloadError::InvalidObservation)?;
    // The unchanged v2 decoder refuses every other unrecognized field.
    Ok((Value::Object(root), at))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn completion_versions_preserve_legacy_bytes_and_refuse_missing_or_false_observations(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ended = json!({"step":1,"completion":"asserted-complete"});
        let legacy = session_step_ended::decode(&ended)?;
        assert_eq!(session_step_ended::encode(&legacy), ended);
        let at = CompletionObservedAt::new(Timestamp::from_millis(300)).ok_or("instant")?;
        let observed = ObservedStep::new(legacy, at);
        assert_eq!(decode_ended(&encode_ended(&observed))?, observed);
        assert!(session_step_ended::decode(&encode_ended(&observed)).is_err());
        assert!(
            decode_ended(&ended).is_err(),
            "v3 accepted an absent completion observation"
        );
        // Use the owning failure constructor, so this witness does not guess its shape.
        let failure = session_step_failed::StepFailedFact::new(
            grapt_types::StepNumber::new(1).ok_or("step")?,
            grapt_types::StepTerminalReason::Interrupted {
                by: grapt_types::InterruptionSource::User,
            },
        )?;
        let observed_failure = ObservedStep::new(failure, at);
        assert_eq!(
            decode_failed(&encode_failed(&observed_failure)?)?,
            observed_failure
        );
        let legacy_failure = session_step_failed::encode(observed_failure.fact())?;
        assert!(
            decode_failed(&legacy_failure).is_err(),
            "v3 failure accepted an absent completion observation"
        );
        for invalid in [
            Value::Null,
            json!(0),
            json!(-1),
            json!(1.5),
            json!("300"),
            json!(u64::MAX),
        ] {
            let mut value = encode_ended(&observed);
            value["completedAt"] = invalid.clone();
            assert!(
                decode_ended(&value).is_err(),
                "v3 accepted an invalid completion observation"
            );
            let mut value = encode_failed(&observed_failure)?;
            value["completedAt"] = invalid;
            assert!(
                decode_failed(&value).is_err(),
                "failure accepted an invalid completion observation"
            );
        }
        let mut unknown = encode_ended(&observed);
        unknown["unreviewed"] = json!(true);
        assert!(decode_ended(&unknown).is_err());
        Ok(())
    }
}
