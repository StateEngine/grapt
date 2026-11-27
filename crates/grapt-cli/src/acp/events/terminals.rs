//! Prompt completion from a live terminal or bounded durable history.
use super::*;

impl AcpService {
    pub(super) async fn terminal_response(
        &self,
        state: &AttachedSession,
        assistant: Option<&str>,
        usage: Option<Value>,
        fallback: &'static str,
    ) -> Result<Value, RpcFault> {
        let context = self.timing.context(state.id.clone());
        drop(context.span(grapt_types::timing::TimingStage::AcpTerminal));
        let finish = if let Some(assistant) = assistant {
            let _fetch = context.span(grapt_types::timing::TimingStage::AcpFinalGet);
            let response = self
                .client()
                .get_value(&format!("api/session/{}/message/{assistant}", state.id))
                .await
                .map_err(remote)?;
            response
                .get("data")
                .and_then(|value| value.get("finish"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        } else {
            None
        };
        let stop = finish_stop(finish.as_deref(), fallback);
        let mut response = json!({"stopReason": stop, "_meta": {}});
        if let Some(value) = usage {
            response["usage"] = value;
        }
        Ok(response)
    }

    /// The prompt's outcome when its terminal event was in a dropped stream:
    /// the newest row after the prompt's input that is either the execution's
    /// failure or an assistant message. A failed execution is projected as a
    /// newer system row, and its earlier Steps may have finished well.
    pub(super) async fn settled_response(
        &self,
        state: &AttachedSession,
        input: &str,
        usage: Option<Value>,
        delivery: &mut crate::acp::backfill::Delivery,
    ) -> Result<Value, RpcFault> {
        let rows = match self.prompt_rows(state, input).await? {
            PromptRows::Rows(rows) => rows,
            PromptRows::Queued(id, text) => {
                self.update(state, notice_update(&id, &text)).await?;
                return Ok(json!({"stopReason": "end_turn", "_meta": {}}));
            }
        };
        self.send_owed(state, &rows, delivery).await?;
        let assistant = rows
            .iter()
            .find(|message| {
                execution_failure(message).is_some()
                    || message.get("type").and_then(Value::as_str) == Some("assistant")
            })
            .ok_or_else(|| RpcFault::internal("prompt settled without an assistant result"))?;
        if let Some(message) = execution_failure(assistant) {
            return Err(RpcFault::internal(message));
        }
        let stop = match assistant.get("error") {
            Some(error) => match failure_stop(error.get("type").and_then(Value::as_str)) {
                Some(stop) => stop,
                None => {
                    return Err(RpcFault::internal(
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("prompt execution failed"),
                    ))
                }
            },
            None => finish_stop(assistant.get("finish").and_then(Value::as_str), "end_turn"),
        };
        let mut response = json!({"stopReason": stop, "_meta": {}});
        if let Some(value) = usage.filter(|_| stop != "cancelled") {
            response["usage"] = value;
        }
        Ok(response)
    }
}
