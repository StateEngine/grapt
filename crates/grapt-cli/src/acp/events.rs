//! Durable event projection, prompt terminals, permission callbacks, and replay.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use super::backfill::PromptRows;
use super::content;
use super::session::{session_id, AttachedSession};
use super::{AcpService, RpcFault};
use crate::client::AdvisoryStream;
use crate::ApiClientError;

mod terminals;

/// Stream rebinds allowed in a row without an event for the prompt's
/// Session. A rebind that delivers nothing is either a silent turn (one idle
/// allowance each) or a server that keeps ending streams at once.
const MAX_QUIET_REBINDS: u8 = 32;

/// What `/compact` answers: the request is durable, and it runs in the next
/// input's first Step (docs/coding-quality/COMPACT-COMMAND.md, option iii).
pub(crate) const COMPACT_NOTICE: &str = "Compaction will run before your next message.";

/// `/compact`, optionally followed by instructions for this summary.
fn is_compact(text: &str) -> bool {
    let text = text.trim();
    text == "/compact" || text.starts_with("/compact ")
}

#[derive(Debug, Default)]
struct ToolState {
    name: String,
    input: Value,
}

impl AcpService {
    pub(crate) async fn prompt(&self, params: Value) -> Result<Value, RpcFault> {
        let session = session_id(&params)?;
        let state = self.require_session(&session).await?;
        let content =
            content::prompt_content(&params, self.client().limits().values().stdin_bytes)?;
        // Built-in names retain their meaning regardless of project catalogs.
        if is_compact(&content.text) {
            return self.compact(&state, &content.text).await;
        }
        let mut events = self.subscribe().await?;
        let mut body = json!({"text": content.text, "delivery": "steer"});
        if !content.files.is_empty() {
            body["files"] = Value::Array(content.files);
        }
        let admitted = self
            .client()
            .post_value(&format!("api/session/{session}/prompt"), &body)
            .await
            .map_err(remote)?;
        let input = admitted
            .get("data")
            .and_then(|value| value.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFault::internal("prompt admission response is malformed"))?;
        // Admitted but blocked before its turn began: it stays queued and no
        // execution follows, so the turn ends here with the reason.
        if let Some(reason) = admitted.pointer("/queued/reason").and_then(Value::as_str) {
            let text = queued_text(reason);
            self.update(&state, notice_update(input, &text)).await?;
            return Ok(json!({"stopReason": "end_turn", "_meta": {}}));
        }
        self.stream_prompt(&state, input, &mut events).await
    }

    async fn subscribe(&self) -> Result<AdvisoryStream, RpcFault> {
        let mut events = self.client().events().await.map_err(remote)?;
        let connected = events.next().await.map_err(remote)?;
        if connected.kind() != "server.connected" {
            return Err(RpcFault::internal("event stream did not establish"));
        }
        Ok(events)
    }

    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn stream_prompt(
        &self,
        state: &AttachedSession,
        input: &str,
        events: &mut AdvisoryStream,
    ) -> Result<Value, RpcFault> {
        let mut started = false;
        let mut assistant = None;
        let mut tools = BTreeMap::<String, ToolState>::new();
        let mut asked = BTreeSet::<String>::new();
        let mut usage = None;
        let mut quiet_rebinds = 0_u8;
        let mut delivery = super::backfill::Delivery::default();
        loop {
            let event = match events.next().await {
                Ok(event) => event,
                // A publication refusal, overflow or transport loss can end
                // this advisory stream; normal execution terminals leave it
                // open. Rebind first, then recover any missing durable text.
                Err(
                    ApiClientError::EventClosed
                    | ApiClientError::Transport(_)
                    | ApiClientError::EventPendingBound { .. },
                ) => {
                    quiet_rebinds = quiet_rebinds.saturating_add(1);
                    if quiet_rebinds > MAX_QUIET_REBINDS {
                        return Err(RpcFault::internal(format!(
                            "event stream ended {MAX_QUIET_REBINDS} times without progress"
                        )));
                    }
                    *events = self.subscribe().await?;
                    delivery.gap();
                    self.pending_permissions(state, &tools, &mut asked).await?;
                    if !self
                        .client()
                        .session_active(&state.id)
                        .await
                        .map_err(remote)?
                    {
                        return self
                            .settled_response(state, input, usage, &mut delivery)
                            .await;
                    }
                    // The promotion may have been in the dropped frames; the
                    // Session is still running this prompt.
                    started = true;
                    continue;
                }
                Err(error) => return Err(remote(error)),
            };
            if event.session_id() != Some(&state.id) {
                continue;
            }
            quiet_rebinds = 0;
            match event.kind() {
                "permission.asked" => {
                    if asked.insert(field(event.data(), "id")?.to_owned()) {
                        self.permission(state, event.data(), &tools).await?;
                    }
                    continue;
                }
                "form.created" => {
                    self.cancel_form(state, event.data()).await;
                    continue;
                }
                "session.input.promoted"
                    if event.data().get("inputID").and_then(Value::as_str) == Some(input) =>
                {
                    if !started {
                        self.mcp_notices(state, input).await?;
                    }
                    started = true;
                    continue;
                }
                _ if !started => continue,
                "session.step.started" => {
                    assistant = event
                        .data()
                        .get("assistantMessageID")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                "session.text.delta" => {
                    if !delivery.had_gap() {
                        let key = super::backfill::Delivery::live_key(
                            event.data(),
                            "agent_message_chunk",
                        )?;
                        delivery
                            .send_text(
                                key,
                                field(event.data(), "delta")?,
                                self.chunk(state, event.data(), "agent_message_chunk"),
                            )
                            .await?;
                    }
                }
                "session.reasoning.delta" => {
                    if !delivery.had_gap() {
                        let key = super::backfill::Delivery::live_key(
                            event.data(),
                            "agent_thought_chunk",
                        )?;
                        delivery
                            .send_text(
                                key,
                                field(event.data(), "delta")?,
                                self.chunk(state, event.data(), "agent_thought_chunk"),
                            )
                            .await?;
                    }
                }
                "session.tool.input.started" => {
                    let call = field(event.data(), "callID")?.to_owned();
                    let name = field(event.data(), "name")?.to_owned();
                    tools.insert(
                        call.clone(),
                        ToolState {
                            name: name.clone(),
                            input: json!({}),
                        },
                    );
                    if delivery.had_gap() || delivery.terminal_sent(&call) {
                        continue;
                    }
                    self.update(
                        state,
                        json!({
                            "sessionUpdate": "tool_call",
                            "toolCallId": call.clone(),
                            "title": name,
                            "kind": tool_kind(&name),
                            "status": "pending",
                            "rawInput": {},
                        }),
                    )
                    .await?;
                    delivery.record_call(&call)?;
                }
                "session.tool.called" => {
                    let call = field(event.data(), "callID")?.to_owned();
                    let input = event
                        .data()
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    let tool = tools.entry(call.clone()).or_insert_with(|| ToolState {
                        name: "tool".to_owned(),
                        input: json!({}),
                    });
                    tool.input = input.clone();
                    let name = tool.name.clone();
                    if delivery.had_gap() || delivery.terminal_sent(&call) {
                        continue;
                    }
                    self.update(
                        state,
                        json!({
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": call,
                            "title": name,
                            "kind": tool_kind(&name),
                            "status": "in_progress",
                            "rawInput": input,
                        }),
                    )
                    .await?;
                }
                "session.tool.progress" => {}
                "session.tool.success" => {
                    let call = field(event.data(), "callID")?.to_owned();
                    let tool = tools.remove(&call).unwrap_or_default();

                    if delivery.had_gap() || delivery.terminal_sent(&call) {
                        continue;
                    }
                    if delivery.claim_sync(&call)? {
                        self.sync_edited_files(state, event.data()).await?;
                    }
                    self.update(
                        state,
                        json!({
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": call.clone(),
                            "title": tool.name,
                            "kind": tool_kind(&tool.name),
                            "status": "completed",
                            "content": tool_content(event.data().get("content")),
                            "rawInput": tool.input,
                            "rawOutput": {"metadata": event.data().get("metadata").cloned().unwrap_or_else(|| json!({}))},
                        }),
                    )
                    .await?;
                    delivery.record_terminal(&call)?;
                }
                "session.tool.failed" => {
                    let call = field(event.data(), "callID")?.to_owned();
                    let tool = tools.remove(&call).unwrap_or_default();
                    let message = event
                        .data()
                        .get("error")
                        .and_then(|value| value.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("Tool failed");
                    if delivery.had_gap() || delivery.terminal_sent(&call) {
                        continue;
                    }
                    self.update(
                        state,
                        json!({
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": call.clone(),
                            "title": tool.name,
                            "kind": tool_kind(&tool.name),
                            "status": "failed",
                            "content": [{"type":"content","content":{"type":"text","text":message}}],
                            "rawInput": tool.input,
                            "rawOutput": {"error": message},
                        }),
                    )
                    .await?;
                    delivery.record_terminal(&call)?;
                }
                "session.usage.updated" => usage = usage_value(event.data()),
                "session.execution.succeeded" => {
                    self.backfill(state, input, &mut delivery).await?;
                    return self
                        .terminal_response(state, assistant.as_deref(), usage, "end_turn")
                        .await;
                }
                "session.execution.interrupted" => {
                    self.backfill(state, input, &mut delivery).await?;
                    return Ok(json!({"stopReason": "cancelled", "_meta": {}}));
                }
                "session.execution.failed" => {
                    self.backfill(state, input, &mut delivery).await?;
                    let error_type = event
                        .data()
                        .get("error")
                        .and_then(|value| value.get("type"))
                        .and_then(Value::as_str);
                    if let Some(stop) = failure_stop(error_type) {
                        if stop == "cancelled" {
                            return Ok(json!({"stopReason": stop, "_meta": {}}));
                        }
                        return self
                            .terminal_response(state, assistant.as_deref(), usage, stop)
                            .await;
                    }
                    let error = event
                        .data()
                        .get("error")
                        .and_then(|value| value.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("prompt execution failed");
                    return Err(RpcFault::internal(error));
                }
                _ => {}
            }
        }
    }

    /// Asks the client about pending permissions a dropped stream never
    /// delivered.
    async fn pending_permissions(
        &self,
        state: &AttachedSession,
        tools: &BTreeMap<String, ToolState>,
        asked: &mut BTreeSet<String>,
    ) -> Result<(), RpcFault> {
        let response = self
            .client()
            .get_value(&format!("api/session/{}/permission", state.id))
            .await
            .map_err(remote)?;
        let requests = response
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| RpcFault::internal("permission list response is malformed"))?;
        for request in requests {
            if request.get("status").and_then(Value::as_str) != Some("pending") {
                continue;
            }
            let data = request
                .as_object()
                .map(|object| object.clone().into_iter().collect::<BTreeMap<_, _>>())
                .ok_or_else(|| RpcFault::internal("permission request is malformed"))?;
            if asked.insert(field(&data, "id")?.to_owned()) {
                self.permission(state, &data, tools).await?;
            }
        }
        Ok(())
    }

    /// The MCP notice this prompt's turn recorded before it planned, shown
    /// ahead of its answer. Best effort: an unreadable page costs the user
    /// the notice, never the prompt; the notice stays in history.
    async fn mcp_notices(&self, state: &AttachedSession, input: &str) -> Result<(), RpcFault> {
        let Ok(response) = self
            .client()
            .get_value_query(
                &format!("api/session/{}/message", state.id),
                &[("order", "desc"), ("limit", "50")],
            )
            .await
        else {
            return Ok(());
        };
        let Some(messages) = response.get("data").and_then(Value::as_array) else {
            return Ok(());
        };
        let notices: Vec<(&str, &str)> = messages
            .iter()
            .take_while(|message| message.get("id").and_then(Value::as_str) != Some(input))
            .filter_map(|message| Some((message.get("id")?.as_str()?, mcp_notice(message)?)))
            .collect();
        for (id, text) in notices.into_iter().rev() {
            self.update(state, notice_update(id, text)).await?;
        }
        Ok(())
    }

    async fn chunk(
        &self,
        state: &AttachedSession,
        data: &BTreeMap<String, Value>,
        kind: &'static str,
    ) -> Result<(), RpcFault> {
        self.update(
            state,
            json!({
                "sessionUpdate": kind,
                "messageId": field(data, "assistantMessageID")?,
                "content": {"type": "text", "text": field(data, "delta")?},
            }),
        )
        .await
    }

    /// Admit a compaction request and end the turn at once: nothing runs now,
    /// so there is no stream to follow.
    async fn compact(&self, state: &AttachedSession, text: &str) -> Result<Value, RpcFault> {
        let instructions = text
            .trim()
            .strip_prefix("/compact")
            .unwrap_or_default()
            .trim();
        let mut body = json!({});
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions);
        }
        self.client()
            .post_value(&format!("api/session/{}/compact", state.id), &body)
            .await
            .map_err(remote)?;
        self.update(
            state,
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": COMPACT_NOTICE},
            }),
        )
        .await?;
        Ok(json!({"stopReason": "end_turn", "_meta": {}}))
    }

    pub(super) async fn update(
        &self,
        state: &AttachedSession,
        update: Value,
    ) -> Result<(), RpcFault> {
        let update =
            super::tool_output::bounded(state.id.as_str(), update, self.peer()?.frame_limit())?;
        self.peer()?
            .notification(
                "session/update",
                json!({"sessionId": state.id.as_str(), "update": update}),
            )
            .await
            .map_err(|_| RpcFault::internal("session update delivery failed"))
    }

    async fn permission(
        &self,
        state: &AttachedSession,
        data: &BTreeMap<String, Value>,
        tools: &BTreeMap<String, ToolState>,
    ) -> Result<(), RpcFault> {
        let request = field(data, "id")?;
        let call = data
            .get("source")
            .and_then(|value| value.get("callID"))
            .and_then(Value::as_str)
            .unwrap_or(request);
        let action = data.get("action").and_then(Value::as_str).unwrap_or("tool");
        let tool = tools.get(call);
        let name = tool.map(|value| value.name.as_str()).unwrap_or(action);
        let result = self
            .peer()?
            .request(
                "session/request_permission",
                json!({
                    "sessionId": state.id.as_str(),
                    "toolCall": {
                        "toolCallId": call,
                        "title": name,
                        "kind": tool_kind(name),
                        "status": "pending",
                        "rawInput": tool.map(|value| value.input.clone()).unwrap_or_else(|| data.get("metadata").cloned().unwrap_or_else(|| json!({}))),
                        "locations": data.get("resources").and_then(Value::as_array).map(|resources| resources.iter().filter_map(Value::as_str).filter(|value| *value != "*").map(|path| json!({"path":path})).collect::<Vec<_>>()).unwrap_or_default(),
                    },
                    "options": [
                        {"optionId":"once","kind":"allow_once","name":"Allow once"},
                        {"optionId":"always","kind":"allow_always","name":"Always allow"},
                        {"optionId":"reject","kind":"reject_once","name":"Reject"},
                    ],
                }),
            )
            .await;
        let reply = result
            .ok()
            .as_ref()
            .and_then(|value| value.get("outcome"))
            .filter(|value| value.get("outcome").and_then(Value::as_str) == Some("selected"))
            .and_then(|value| value.get("optionId"))
            .and_then(Value::as_str)
            .filter(|value| matches!(*value, "once" | "always"))
            .unwrap_or("reject")
            .to_owned();
        self.client()
            .post_empty(
                &format!("api/session/{}/permission/{request}/reply", state.id),
                &json!({"reply": reply}),
            )
            .await
            .map_err(remote)
    }

    async fn cancel_form(&self, state: &AttachedSession, data: &BTreeMap<String, Value>) {
        let form = data
            .get("form")
            .and_then(|value| value.get("id"))
            .and_then(Value::as_str);
        if let Some(form) = form {
            let result = self
                .client()
                .post_empty(
                    &format!("api/session/{}/form/{form}/cancel", state.id),
                    &json!({}),
                )
                .await;
            if result.is_err() {
                let _interrupt = self.client().interrupt(&state.id).await;
            }
        }
    }

    pub(super) async fn sync_edited_files(
        &self,
        state: &AttachedSession,
        data: &BTreeMap<String, Value>,
    ) -> Result<(), RpcFault> {
        if !self.client_writes_files() {
            return Ok(());
        }
        let files = data
            .get("metadata")
            .and_then(|value| value.get("files"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for file in files {
            let Some(raw) = file.get("file").and_then(Value::as_str) else {
                continue;
            };
            let path = std::path::Path::new(raw);
            let target = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::path::Path::new(&state.cwd).join(path)
            };
            let root = std::path::PathBuf::from(&state.cwd);
            let maximum = self.client().limits().values().stdin_bytes;
            let read = tokio::task::spawn_blocking(move || {
                let root = std::fs::canonicalize(root).ok()?;
                let target = std::fs::canonicalize(target).ok()?;
                if !target.starts_with(&root) {
                    return None;
                }
                let metadata = std::fs::metadata(&target).ok()?;
                let size = usize::try_from(metadata.len()).ok()?;
                if size > maximum {
                    return None;
                }
                std::fs::read_to_string(&target)
                    .ok()
                    .map(|content| (target, content))
            })
            .await;
            let Ok(Some((target, content))) = read else {
                continue;
            };
            let Some(path) = target.to_str() else {
                continue;
            };
            self.peer()?
                .request(
                    "fs/write_text_file",
                    json!({"sessionId": state.id.as_str(), "path": path, "content": content}),
                )
                .await
                .map_err(|fault| {
                    RpcFault::internal(format!(
                        "edited file synchronization failed: {}",
                        fault.message
                    ))
                })?;
        }
        Ok(())
    }
}

fn field<'a>(data: &'a BTreeMap<String, Value>, key: &'static str) -> Result<&'a str, RpcFault> {
    data.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| RpcFault::internal(format!("event {key} is malformed")))
}

pub(super) fn tool_kind(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "bash" | "shell" => "execute",
        "webfetch" => "fetch",
        "edit" | "apply_patch" | "patch" | "write" => "edit",
        "grep" | "glob" | "context" => "search",
        "read" => "read",
        "task" | "subagent" => "think",
        _ => "other",
    }
}

pub(super) fn tool_content(value: Option<&Value>) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .map(|content| {
            content
                .iter()
                .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                    Some("text") => Some(json!({"type":"content","content":{"type":"text","text":item.get("text")?}})),
                    Some("file") => Some(json!({"type":"content","content":{"type":"resource_link","uri":item.get("uri")?,"name":item.get("name").and_then(Value::as_str).unwrap_or("file"),"mimeType":item.get("mime")?}})),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn usage_value(data: &BTreeMap<String, Value>) -> Option<Value> {
    let tokens = data.get("tokens")?.as_object()?;
    let input = nonnegative(tokens, "input")?;
    let output = nonnegative(tokens, "output")?;
    let thought = nonnegative(tokens, "reasoning").unwrap_or(0);
    let cache = tokens.get("cache").and_then(Value::as_object);
    let read = cache
        .and_then(|value| nonnegative(value, "read"))
        .unwrap_or(0);
    let write = cache
        .and_then(|value| nonnegative(value, "write"))
        .unwrap_or(0);
    let mut value = Map::new();
    value.insert("inputTokens".to_owned(), json!(input));
    value.insert("outputTokens".to_owned(), json!(output));
    value.insert(
        "totalTokens".to_owned(),
        json!(input
            .saturating_add(output)
            .saturating_add(thought)
            .saturating_add(read)
            .saturating_add(write)),
    );
    if thought > 0 {
        value.insert("thoughtTokens".to_owned(), json!(thought));
    }
    if read > 0 {
        value.insert("cachedReadTokens".to_owned(), json!(read));
    }
    if write > 0 {
        value.insert("cachedWriteTokens".to_owned(), json!(write));
    }
    Some(Value::Object(value))
}

fn nonnegative(value: &Map<String, Value>, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

/// What one history page, newest-first, holds for a settling prompt.
#[derive(Debug, PartialEq)]
pub(super) enum SettlePage<'a> {
    /// The prompt itself, at this index: the rows before it are its own.
    Prompt(usize),
    /// The prompt is queued: its own not-run-yet notice's id and text.
    Queued(&'a str, &'a str),
    /// Neither: rows newer than the prompt, or another prompt's.
    Older,
}

pub(super) fn settle_page<'a>(page: &'a [Value], input: &str) -> SettlePage<'a> {
    if let Some(position) = page
        .iter()
        .position(|message| message.get("id").and_then(Value::as_str) == Some(input))
    {
        return SettlePage::Prompt(position);
    }
    // A prompt that ran later is newer than its notice, so meeting the
    // notice first means the prompt is still queued.
    page.iter()
        .find_map(|message| queued_notice(message, input))
        .map_or(SettlePage::Older, |(id, text)| SettlePage::Queued(id, text))
}

/// History pages a settle reads back to its prompt, as `run` does.
pub(super) const MAX_SETTLE_PAGES: u16 = 200;

/// The text of a durable MCP notice row.
fn mcp_notice(message: &Value) -> Option<&str> {
    if message.get("type").and_then(Value::as_str) != Some("system")
        || message.pointer("/metadata/mcpUnavailable") != Some(&Value::Bool(true))
    {
        return None;
    }
    message.get("text").and_then(Value::as_str)
}

/// The id and text of the not-run-yet notice for `input`, a prompt that
/// stays queued because its turn was blocked: an exact messageID match only.
fn queued_notice<'a>(message: &'a Value, input: &str) -> Option<(&'a str, &'a str)> {
    if message.get("type").and_then(Value::as_str) != Some("system")
        || message.pointer("/metadata/notRunYet") != Some(&Value::Bool(true))
        || message
            .pointer("/metadata/messageID")
            .and_then(Value::as_str)
            != Some(input)
    {
        return None;
    }
    Some((message.get("id")?.as_str()?, message.get("text")?.as_str()?))
}

/// The not-run-yet words for a queued prompt's reason, as history shows them.
fn queued_text(reason: &str) -> String {
    format!("Not run yet: {reason}. Your message is queued and runs on the next turn once this is fixed.")
}

/// A notice as ACP shows product text: an agent chunk marked as the system's.
fn notice_update(id: &str, text: &str) -> Value {
    json!({
        "sessionUpdate": "agent_message_chunk",
        "messageId": id,
        "content": {"type": "text", "text": format!("[system] {text}")},
    })
}

/// The message of a durable execution failure row, as the live
/// `session.execution.failed` event carries it.
fn execution_failure(message: &Value) -> Option<&str> {
    if message.get("type").and_then(Value::as_str) != Some("system")
        || message.pointer("/metadata/executionFailure") != Some(&Value::Bool(true))
    {
        return None;
    }
    let text = message.get("text").and_then(Value::as_str)?;
    Some(text.strip_prefix("Execution failed: ").unwrap_or(text))
}

fn finish_stop(finish: Option<&str>, fallback: &'static str) -> &'static str {
    match finish {
        Some("length") => "max_tokens",
        Some("content-filter") => "refusal",
        _ => fallback,
    }
}

fn failure_stop(error_type: Option<&str>) -> Option<&'static str> {
    match error_type {
        Some("aborted") => Some("cancelled"),
        Some("provider.content-filter") => Some("refusal"),
        _ => None,
    }
}

pub(super) fn remote(error: crate::ApiClientError) -> RpcFault {
    RpcFault::internal(format!("ordinary API request failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        failure_stop, finish_stop, mcp_notice, notice_update, queued_notice, queued_text,
        settle_page, SettlePage,
    };
    use serde_json::json;

    /// Review P-H1: a settle walks page by page; another
    /// prompt's answer on a newer page is never borrowed, this prompt's own
    /// queued notice on an older page is found, another prompt's is not, and
    /// a prompt that ran later is found before its old notice.
    #[test]
    fn a_settle_walks_to_its_own_prompt_or_queued_notice() {
        let earlier = json!({"type":"assistant","id":"msg_earlier","text":"another answer"});
        assert_eq!(
            settle_page(std::slice::from_ref(&earlier), "msg_input"),
            SettlePage::Older
        );
        let notice = json!({"type":"system","id":"msg_q",
            "metadata":{"notRunYet":true,"messageID":"msg_input"},"text":"Not run yet: x."});
        assert_eq!(
            settle_page(std::slice::from_ref(&notice), "msg_input"),
            SettlePage::Queued("msg_q", "Not run yet: x.")
        );
        assert_eq!(
            settle_page(std::slice::from_ref(&notice), "msg_other"),
            SettlePage::Older
        );
        let promoted = [earlier, json!({"type":"user","id":"msg_input"}), notice];
        assert_eq!(settle_page(&promoted, "msg_input"), SettlePage::Prompt(1));
        assert!(queued_text("a reason").starts_with("Not run yet: a reason. "));
    }

    #[test]
    fn only_an_mcp_notice_row_is_shown_and_as_the_systems_words() {
        let notice = json!({"type":"system","id":"msg_a","metadata":{"mcpUnavailable":true},
            "text":"MCP servers are available again."});
        assert_eq!(
            mcp_notice(&notice),
            Some("MCP servers are available again.")
        );
        let queued = json!({"type":"system","id":"msg_q",
            "metadata":{"notRunYet":true,"messageID":"msg_input"},
            "text":"Not run yet: x. Your message is queued and runs on the next turn once this is fixed."});
        assert_eq!(
            mcp_notice(&queued),
            None,
            "a not-run-yet notice is not an MCP notice"
        );
        assert_eq!(
            queued_notice(&queued, "msg_input"),
            Some((
                "msg_q",
                "Not run yet: x. Your message is queued and runs on the next turn once this is fixed."
            ))
        );
        assert_eq!(
            queued_notice(&queued, "msg_other"),
            None,
            "another prompt's notice is not this prompt's"
        );
        let failure = json!({"type":"system","id":"msg_b","metadata":{"executionFailure":true},
            "text":"Execution failed: x"});
        assert_eq!(mcp_notice(&failure), None);
        let assistant = json!({"type":"assistant","metadata":{"mcpUnavailable":true},"text":"x"});
        assert_eq!(mcp_notice(&assistant), None);
        assert_eq!(
            notice_update("msg_a", "MCP servers are available again."),
            json!({"sessionUpdate":"agent_message_chunk","messageId":"msg_a",
                "content":{"type":"text","text":"[system] MCP servers are available again."}})
        );
    }

    #[test]
    fn terminal_reasons_cover_every_advertised_acp_outcome() {
        assert_eq!(finish_stop(None, "end_turn"), "end_turn");
        assert_eq!(finish_stop(Some("stop"), "end_turn"), "end_turn");
        assert_eq!(
            finish_stop(Some("length"), "end_turn"),
            "max_tokens",
            "length finish lost the ACP max_tokens reason"
        );
        assert_eq!(finish_stop(Some("content-filter"), "end_turn"), "refusal");
        assert_eq!(failure_stop(Some("aborted")), Some("cancelled"));
        assert_eq!(
            failure_stop(Some("provider.content-filter")),
            Some("refusal")
        );
        assert_eq!(failure_stop(Some("provider.error")), None);
    }
}
