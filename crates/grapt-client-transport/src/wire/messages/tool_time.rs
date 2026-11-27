//! Preserve reported tool duration; missing or reversed timestamps stay unknown.
use super::*;
use grapt_api::u3a::SessionMessageAssistantToolDtoTime;
use std::time::Duration;

pub(super) fn decode(time: &SessionMessageAssistantToolDtoTime) -> Result<Option<u64>, WireError> {
    let value = serde_json::to_value(time).map_err(|error| WireError::Invalid {
        what: "tool timestamps",
        value: error.to_string(),
    })?;
    let (Some(ran), Some(completed)) = (
        value.get("ran").and_then(serde_json::Value::as_f64),
        value.get("completed").and_then(serde_json::Value::as_f64),
    ) else {
        return Ok(None);
    };
    Ok((ran >= 0.0 && completed >= ran)
        .then(|| Duration::try_from_secs_f64((completed - ran) / 1000.0).ok())
        .flatten()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok()))
}

pub(super) fn encode(
    duration_ms: Option<u64>,
) -> Result<SessionMessageAssistantToolDtoTime, WireError> {
    let created = 1_700_000_000_000.0;
    let (ran, completed) = if let Some(duration) = duration_ms {
        (
            Optional::present(finite(created)?),
            Optional::present(finite(created + duration as f64)?),
        )
    } else {
        (Optional::absent(), Optional::absent())
    };
    Ok(SessionMessageAssistantToolDtoTime::new(
        Required::new(finite(created)?),
        ran,
        completed,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_reversed_zero_and_elapsed_timestamps_remain_distinct() -> Result<(), WireError> {
        let missing = SessionMessageAssistantToolDtoTime::new(
            Required::new(finite(1000.0)?),
            Optional::absent(),
            Optional::absent(),
        );
        assert_eq!(decode(&missing)?, None);
        for (ran, completed, expected) in [
            (1000.0, 1850.0, Some(850)),
            (1000.0, 1000.0, Some(0)),
            (1000.0, 999.0, None),
        ] {
            let time = SessionMessageAssistantToolDtoTime::new(
                Required::new(finite(1000.0)?),
                Optional::present(finite(ran)?),
                Optional::present(finite(completed)?),
            );
            assert_eq!(decode(&time)?, expected);
        }
        for duration in [None, Some(0), Some(850), Some(12345)] {
            assert_eq!(decode(&encode(duration)?)?, duration);
        }
        Ok(())
    }
}
