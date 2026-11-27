//! An advisory record of the context actually omitted from one request.
use super::*;
use grapt_types::EventSeq;
pub(super) const CONTEXT_TRIMMED: &str = "context-trimmed";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEstimate {
    pub before: u64,
    pub after: u64,
    pub output_reserve: u64,
    /// Effective context capacity used by admission after account limits.
    /// This can be smaller than the model's advertised context window.
    pub budget: u64,
}
impl ContextEstimate {
    /// `before` and `after` are both the encoded request's own size. Without
    /// a window floor the request itself shrank; with one, the history before
    /// the floor was omitted and the request may not have shrunk further.
    fn validate(self, window_floor: Option<EventSeq>) -> Result<(), String> {
        let shrank = match window_floor {
            Some(_) => self.before >= self.after,
            None => self.before > self.after,
        };
        if !shrank
            || self.after == 0
            || self.output_reserve == 0
            || self.budget > 9_007_199_254_740_991
            || self.before > 64 * 1024 * 1024
            || self
                .after
                .checked_add(self.output_reserve)
                .is_none_or(|total| total > self.budget)
        {
            return Err("context trim estimate is invalid".into());
        }
        Ok(())
    }
    pub fn metadata(self) -> serde_json::Value {
        serde_json::json!({"basis":"encoded-request-bytes", "before":self.before,"after":self.after,"outputReserve":self.output_reserve,"budget":self.budget,"budgetBasis":"admission-window"})
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextTrimNotice {
    pub position: CommittedPosition,
    pub execution: ExecutionId,
    pub estimate: ContextEstimate,
    /// Where the request's history began, when everything before it was
    /// omitted because it lay past what one request can read.
    pub window_floor: Option<EventSeq>,
}
#[derive(Serialize, Deserialize)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    kind: String,
    estimate: ContextEstimate,
    #[serde(
        rename = "windowFloor",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    window_floor: Option<u64>,
}
pub fn record_context_trimmed(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    estimate: ContextEstimate,
    window_floor: Option<EventSeq>,
) -> Result<ContextTrimNotice, String> {
    estimate.validate(window_floor)?;
    let data = Data {
        session: session.as_str().into(),
        execution: execution.as_str().into(),
        kind: CONTEXT_TRIMMED.into(),
        estimate,
        window_floor: window_floor.map(EventSeq::value),
    };
    let position = record_data_notice(
        store,
        authorities,
        session,
        &serde_json::to_value(data).map_err(|error| error.to_string())?,
    )?;
    Ok(ContextTrimNotice {
        position,
        execution: execution.clone(),
        estimate,
        window_floor,
    })
}
pub(super) fn at(
    connection: &rusqlite::Connection,
    session: &SessionId,
    seq: Option<u64>,
) -> Result<Vec<ContextTrimNotice>, String> {
    kind_rows_at::<Data>(connection, session, CONTEXT_TRIMMED, seq)?
        .into_iter()
        .map(|(position, data)| {
            if data.session != session.as_str() || data.kind != CONTEXT_TRIMMED {
                return Err("context trim notice identity is invalid".into());
            }
            let window_floor = data.window_floor.map(EventSeq::new);
            data.estimate.validate(window_floor)?;
            let execution = ExecutionId::new(data.execution)
                .ok_or("context trim execution identity is invalid")?;
            Ok(ContextTrimNotice {
                position,
                execution,
                estimate: data.estimate,
                window_floor,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn estimate(before: u64, after: u64) -> ContextEstimate {
        ContextEstimate {
            before,
            after,
            output_reserve: 100,
            budget: 10_000,
        }
    }

    /// The estimate stays the encoded request's own size; history omitted
    /// before a window is named by its floor, never added into `before`.
    #[test]
    fn omitted_history_is_named_apart_from_the_request_estimate() -> Result<(), String> {
        let unshrunk = estimate(4_000, 4_000);
        assert!(unshrunk.validate(None).is_err(), "nothing was omitted");
        unshrunk.validate(Some(EventSeq::new(42)))?;
        estimate(4_000, 3_000).validate(None)?;
        assert!(estimate(3_000, 4_000)
            .validate(Some(EventSeq::new(42)))
            .is_err());
        let data = Data {
            session: "ses_trim".into(),
            execution: "exe_trim".into(),
            kind: CONTEXT_TRIMMED.into(),
            estimate: unshrunk,
            window_floor: Some(42),
        };
        let value = serde_json::to_value(&data).map_err(|error| error.to_string())?;
        assert_eq!(value["estimate"]["before"], 4_000);
        assert_eq!(value["estimate"]["after"], 4_000);
        assert_eq!(value["windowFloor"], 42);
        let plain = Data {
            window_floor: None,
            ..data
        };
        let value = serde_json::to_value(&plain).map_err(|error| error.to_string())?;
        assert!(
            value.get("windowFloor").is_none(),
            "an absent floor is not written"
        );
        Ok(())
    }
}
