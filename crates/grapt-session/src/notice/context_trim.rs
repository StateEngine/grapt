//! An advisory record of the context actually omitted from one request.
use super::*;
pub(super) const CONTEXT_TRIMMED: &str = "context-trimmed";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ContextEstimate {
    pub before: u64,
    pub after: u64,
    pub output_reserve: u64,
    /// Effective context capacity used by admission after account limits.
    /// This can be smaller than the model's advertised context window.
    pub budget: u64,
}
impl ContextEstimate {
    fn validate(self) -> Result<(), String> {
        if self.before <= self.after
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
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    #[serde(rename = "sessionID")]
    session: String,
    #[serde(rename = "executionID")]
    execution: String,
    kind: String,
    estimate: ContextEstimate,
}
pub fn record_context_trimmed(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    estimate: ContextEstimate,
) -> Result<ContextTrimNotice, String> {
    estimate.validate()?;
    let data = Data {
        session: session.as_str().into(),
        execution: execution.as_str().into(),
        kind: CONTEXT_TRIMMED.into(),
        estimate,
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
            data.estimate.validate()?;
            let execution = ExecutionId::new(data.execution)
                .ok_or("context trim execution identity is invalid")?;
            Ok(ContextTrimNotice {
                position,
                execution,
                estimate: data.estimate,
            })
        })
        .collect()
}
