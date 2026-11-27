//! Bounded, ordered initial-load display; never re-execution of historical effects.
//!
//! Copyright 2023-2026 SpaceXAI.
//! Apache-2.0; see THIRD_PARTY_NOTICES.md. Modified by Grapt: API pagination,
//! serial writer acknowledgements and historical ACP metadata, without JSONL
//! storage or a second application-acknowledgement protocol.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::backfill::chunks;
use super::events::{remote, tool_content, tool_kind};
use super::session::AttachedSession;
use super::{AcpService, RpcFault};

/// The step-finish `_meta` key from before the product rename.
const LEGACY_FINISH_META: &str = "secode/finish"; // grapt-rename: keep

const MAX_REPLAY_PAGES: usize = 200;
const MAX_CURSOR_BYTES: usize = 8192;

impl AcpService {
    pub(crate) async fn replay(&self, state: &AttachedSession) -> Result<(), RpcFault> {
        let path = format!("api/session/{}/message", state.id);
        let mut cursor: Option<String> = None;
        let mut seen = BTreeSet::new();
        for _ in 0..MAX_REPLAY_PAGES {
            let query = match cursor.as_deref() {
                Some(cursor) => vec![("cursor", cursor)],
                None => vec![("order", "asc"), ("limit", "200")],
            };
            let response = self
                .client()
                .get_value_query(&path, &query)
                .await
                .map_err(remote)?;
            let messages = response
                .get("data")
                .and_then(Value::as_array)
                .ok_or_else(|| RpcFault::internal("message replay response is malformed"))?;
            let next = match response.get("cursor") {
                None => None,
                Some(Value::Object(cursor)) => cursor.get("next"),
                _ => return Err(RpcFault::internal("message replay cursor is malformed")),
            };
            let next = match next {
                None | Some(Value::Null) => None,
                Some(Value::String(next)) if !next.is_empty() && next.len() <= MAX_CURSOR_BYTES => {
                    Some(next)
                }
                _ => return Err(RpcFault::internal("message replay cursor is malformed")),
            };
            // As in the native client, reject no-progress pages BEFORE applying
            // their rows. Also catch longer cycles within the finite page budget.
            if next.is_some_and(|next| seen.contains(next)) {
                return Err(RpcFault::internal("message replay cursor made no progress"));
            }
            for message in messages {
                self.replay_message(state, message).await?;
            }
            match next {
                Some(next) if !messages.is_empty() => {
                    seen.insert(next.clone());
                    cursor = Some(next.clone());
                }
                _ => return Ok(()),
            }
        }
        Err(RpcFault::internal(
            "message replay exceeded its page bound; history is incomplete",
        ))
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn replay_message(
        &self,
        state: &AttachedSession,
        message: &Value,
    ) -> Result<(), RpcFault> {
        let id = text(message, "id")?;
        match text(message, "type")? {
            "user" => {
                self.replay_text(state, id, "user_message_chunk", text(message, "text")?)
                    .await?
            }
            "assistant" => {
                let parts = message
                    .get("content")
                    .and_then(Value::as_array)
                    .ok_or_else(|| RpcFault::internal("assistant replay content is malformed"))?;
                for part in parts {
                    match text(part, "type")? {
                        "text" => {
                            self.replay_text(state, id, "agent_message_chunk", text(part, "text")?)
                                .await?
                        }
                        "reasoning" => {
                            self.replay_text(state, id, "agent_thought_chunk", text(part, "text")?)
                                .await?
                        }
                        "tool" => self.replay_tool(state, part).await?,
                        _ => return Err(RpcFault::internal("unsupported assistant replay part")),
                    }
                }
                if let Some(error) = message.get("error").filter(|value| !value.is_null()) {
                    self.replay_notice(
                        state,
                        id,
                        &format!("Assistant failed: {}", text(error, "message")?),
                    )
                    .await?;
                }
                if let Some(finish) = message.get("finish").filter(|value| !value.is_null()) {
                    let finish = finish
                        .as_str()
                        .filter(|finish| {
                            matches!(
                                *finish,
                                "stop"
                                    | "length"
                                    | "tool-calls"
                                    | "content-filter"
                                    | "error"
                                    | "unknown"
                            )
                        })
                        .ok_or_else(|| {
                            RpcFault::internal("history assistant finish is malformed")
                        })?;
                    // Preserve Step completion without adding transcript prose
                    // or interpreting tool-calls as an execution terminal. The
                    // pre-rename key is sent beside the current one for a release.
                    self.replay_update(
                        state,
                        json!({"sessionUpdate":"agent_message_chunk",
                        "messageId":id,"content":{"type":"text","text":""},
                        "_meta":{"grapt/finish":finish, LEGACY_FINISH_META:finish}}),
                    )
                    .await?;
                }
            }
            "system" | "synthetic" => {
                self.replay_notice(state, id, text(message, "text")?)
                    .await?
            }
            "skill" => {
                self.replay_notice(
                    state,
                    id,
                    &format!(
                        "Skill {}: {}",
                        text(message, "name")?,
                        text(message, "text")?
                    ),
                )
                .await?
            }
            "agent-switched" => {
                self.replay_notice(
                    state,
                    id,
                    &format!("Agent changed to {}.", text(message, "agent")?),
                )
                .await?
            }
            "model-switched" => {
                let model = message
                    .get("model")
                    .ok_or_else(|| RpcFault::internal("history model is missing"))?;
                self.replay_notice(
                    state,
                    id,
                    &format!(
                        "Model changed to {}/{}.",
                        text(model, "providerID")?,
                        text(model, "id")?
                    ),
                )
                .await?;
            }
            "compaction" => {
                let notice = match text(message, "status")? {
                    "running" => "Compaction running.".to_owned(),
                    "completed" => format!(
                        "Compaction completed.\nSummary: {}",
                        text(message, "summary")?
                    ),
                    "failed" => {
                        format!("Compaction failed: {}", text(&message["error"], "message")?)
                    }
                    _ => return Err(RpcFault::internal("history compaction status is malformed")),
                };
                self.replay_notice(state, id, &notice).await?;
            }
            "shell" => {
                self.replay_notice(
                    state,
                    id,
                    &format!(
                        "Shell {}: {}",
                        text(message, "status")?,
                        text(message, "command")?
                    ),
                )
                .await?;
                if let Some(output) = message.pointer("/output/output").and_then(Value::as_str) {
                    self.replay_text(state, id, "agent_message_chunk", output)
                        .await?;
                }
                if message.pointer("/output/truncated") == Some(&Value::Bool(true)) {
                    self.replay_notice(state, id, "Shell output is truncated in recorded history.")
                        .await?;
                }
            }
            _ => return Err(RpcFault::internal("unsupported history message type")),
        }
        Ok(())
    }

    async fn replay_notice(
        &self,
        state: &AttachedSession,
        id: &str,
        notice: &str,
    ) -> Result<(), RpcFault> {
        // Boundaries belong to generated notices; recorded output stays exact.
        self.replay_text(
            state,
            id,
            "agent_message_chunk",
            &format!("\n[system] {notice}\n"),
        )
        .await
    }

    async fn replay_text(
        &self,
        state: &AttachedSession,
        id: &str,
        kind: &str,
        value: &str,
    ) -> Result<(), RpcFault> {
        for chunk in chunks(value) {
            self.replay_update(
                state,
                json!({"sessionUpdate":kind,"messageId":id,
                "content":{"type":"text","text":chunk}}),
            )
            .await?;
        }
        Ok(())
    }

    async fn replay_tool(&self, state: &AttachedSession, part: &Value) -> Result<(), RpcFault> {
        let call = text(part, "id")?;
        let name = text(part, "name")?;
        let snapshot = part
            .get("state")
            .ok_or_else(|| RpcFault::internal("history tool state is missing"))?;
        let status = match text(snapshot, "status")? {
            "streaming" => "pending",
            "running" => "in_progress",
            "completed" => "completed",
            "error" => "failed",
            _ => return Err(RpcFault::internal("history tool status is malformed")),
        };
        let input = snapshot
            .get("input")
            .cloned()
            .ok_or_else(|| RpcFault::internal("history tool input is missing"))?;
        self.replay_update(
            state,
            json!({"sessionUpdate":"tool_call","toolCallId":call,
            "title":name,"kind":tool_kind(name),"status":"pending","rawInput":input}),
        )
        .await?;
        if status == "pending" {
            return Ok(());
        }
        let mut content = tool_content(snapshot.get("content"));
        if content.is_empty() && status == "failed" {
            content.push(json!({"type":"content","content":{"type":"text","text":text(&snapshot["error"], "message")?}}));
        }
        // Replaying a historical edit must never invoke fs/write_text_file,
        // request permission, or repeat any other effect from live recovery.
        self.replay_update(
            state,
            json!({"sessionUpdate":"tool_call_update","toolCallId":call,
            "title":name,"kind":tool_kind(name),"status":status,"rawInput":input,"content":content,
            "rawOutput":if status == "failed" {json!({"error":snapshot.get("error")})}
                else {json!({"metadata":snapshot.get("metadata")})}}),
        )
        .await
    }

    async fn replay_update(&self, state: &AttachedSession, update: Value) -> Result<(), RpcFault> {
        // Reserve the small, fixed historical metadata envelope in the same
        // tool-output budget. Await the existing writer receipt per update;
        // flush completion is deliberately not called client application.
        let update = super::tool_output::bounded(
            state.id.as_str(),
            update,
            self.peer()?.frame_limit().saturating_sub(64),
        )?;
        self.peer()?
            .notification(
                "session/update",
                json!({"sessionId":state.id.as_str(),
            "update":update,"_meta":{"isReplay":true}}),
            )
            .await
            .map_err(|_| RpcFault::internal("history update delivery failed"))
    }
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str, RpcFault> {
    value
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcFault::internal(format!("history {name} is malformed")))
}
