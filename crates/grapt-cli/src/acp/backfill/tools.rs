//! Final history snapshots are display recovery, never tool execution.
use super::*;
use crate::acp::events::{tool_content, tool_kind};

impl AcpService {
    pub(super) async fn recover_tool(
        &self,
        state: &AttachedSession,
        part: &Value,
        delivery: &mut Delivery,
    ) -> Result<(), RpcFault> {
        let call = part
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFault::internal("history tool identity is malformed"))?;
        if delivery.terminal_sent(call) {
            return Ok(());
        }
        let name = part
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFault::internal("history tool name is malformed"))?;
        let snapshot = part
            .get("state")
            .ok_or_else(|| RpcFault::internal("history tool state is missing"))?;
        let status = match snapshot.get("status").and_then(Value::as_str) {
            Some("completed") => "completed",
            Some("error") => "failed",
            _ => {
                return Err(RpcFault::internal(
                    "settled history tool has no final outcome",
                ))
            }
        };
        let input = snapshot.get("input").cloned().unwrap_or_else(|| json!({}));
        if !delivery.call_sent(call) {
            self.update(
                state,
                json!({
                    "sessionUpdate":"tool_call", "toolCallId":call,
                    "title":name, "kind":tool_kind(name), "status":"pending", "rawInput":input,
                }),
            )
            .await?;
            delivery.record_call(call)?;
        }
        if status == "completed" && delivery.claim_sync(call)? {
            let data = std::collections::BTreeMap::from([(
                "metadata".to_owned(),
                snapshot
                    .get("metadata")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            )]);
            self.sync_edited_files(state, &data).await?;
        }
        let mut content = tool_content(snapshot.get("content"));
        if content.is_empty() && status == "failed" {
            let error = snapshot
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Tool failed");
            content.push(json!({"type":"content", "content":{"type":"text", "text":error}}));
        }
        self.update(
            state,
            json!({
                "sessionUpdate":"tool_call_update", "toolCallId":call,
                "title":name, "kind":tool_kind(name), "status":status,
                "content":content, "rawInput":input,
                "rawOutput":if status == "failed" {
                    json!({"error":snapshot.pointer("/error/message")})
                } else { json!({"metadata":snapshot.get("metadata")}) },
            }),
        )
        .await?;
        delivery.record_terminal(call)?;
        Ok(())
    }
}
