//! Native Converse request projection and event decoding.
//! License and attribution notices are retained in THIRD_PARTY_NOTICES.md.
use crate::{BedrockConverseRequest, CodexInputItem, MessageRole, ToolChoice};
use serde_json::{json, Value};

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn body(request: &BedrockConverseRequest) -> Result<Value, &'static str> {
    let mut messages: Vec<Value> = Vec::new();
    let mut system = Vec::new();
    for item in &request.messages {
        let (role, content) = match item {
            CodexInputItem::AssistantResponseStart => continue,
            CodexInputItem::Message(message) if message.role == MessageRole::System => {
                system.push(json!({"text":message.content}));
                continue;
            }
            CodexInputItem::Message(message) => (
                if message.role == MessageRole::Assistant {
                    "assistant"
                } else {
                    "user"
                },
                vec![json!({"text":message.content})],
            ),
            CodexInputItem::ToolCall {
                call,
                tool,
                arguments,
            } => (
                "assistant",
                vec![
                    json!({"toolUse":{"toolUseId":call.as_str(),"name":tool.as_str(),
                    "input":arguments.object_value()}}),
                ],
            ),
            CodexInputItem::TextToolCall { call, tool, input } => (
                "assistant",
                vec![
                    json!({"toolUse":{"toolUseId":call.as_str(),"name":tool.as_str(),"input":{"input":input.as_str()}}}),
                ],
            ),
            CodexInputItem::ToolOutput {
                call,
                output,
                is_error,
            } => (
                "user",
                vec![
                    json!({"toolResult":{"toolUseId":call.as_str(),"content":[{"text":output.as_str()}],
                    "status":if *is_error { "error" } else { "success" }}}),
                ],
            ),
            CodexInputItem::ImageUser { text, images } => {
                let mut content = vec![json!({"text":text.as_str()})];
                content.extend(image_content(images));
                ("user", content)
            }
            CodexInputItem::ImageToolOutput {
                call,
                output,
                images,
            } => {
                let mut content = vec![json!({"text":output.as_str()})];
                content.extend(image_content(images));
                (
                    "user",
                    vec![
                        json!({"toolResult":{"toolUseId":call.as_str(),"content":content,"status":"success"}}),
                    ],
                )
            }
            CodexInputItem::ProviderReplay(replay) => (
                "assistant",
                vec![replay
                    .bedrock_block()
                    .ok_or("Bedrock replay requires its original protocol")?],
            ),
            CodexInputItem::EffortUpdate(_) => {
                return Err("Bedrock effort updates are not supported")
            }
            CodexInputItem::CompatibleReasoning(_) => {
                return Err("compatible reasoning requires its declared protocol")
            }
            CodexInputItem::CompactionTrigger => {
                return Err("Bedrock does not support Responses compaction")
            }
        };
        if let Some(last) = messages.last_mut().filter(|last| last["role"] == role) {
            last["content"]
                .as_array_mut()
                .ok_or("invalid Bedrock message")?
                .extend(content);
        } else {
            messages.push(json!({"role":role,"content":content}));
        }
    }
    let mut body =
        json!({"messages":messages,"inferenceConfig":{"maxTokens":request.max_tokens.value()}});
    if crate::replay::bedrock::binding_controls(request.model.model().as_str()) {
        body["additionalModelRequestFields"] = json!({
            "thinking":{"type":"adaptive","block_binding":{"prefix_mismatch_behavior":"drop_block"}},
            "anthropic_beta":["thinking-binding-controls-2026-08-01"]
        });
    }
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if !request.tools.is_empty() {
        let tools = request
            .tools
            .iter()
            .map(|tool| {
                let hint = super::anthropic_tools::project(tool)?;
                let mut spec =
                    json!({"name":tool.name,"inputSchema":{"json":hint["input_schema"]}});
                if let Some(description) = hint.get("description") {
                    spec["description"] = description.clone();
                }
                Ok(json!({"toolSpec":spec}))
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        body["toolConfig"] = json!({"tools":tools});
        // Converse has no native "none". History still requires its tool
        // definitions; the summary owner refuses any returned tool effect.
        if request.tool_choice != Some(ToolChoice::None) {
            body["toolConfig"]["toolChoice"] = match &request.tool_choice {
                Some(ToolChoice::Required) => json!({"any":{}}),
                Some(ToolChoice::Named { tool }) => json!({"tool":{"name":tool}}),
                _ => json!({"auto":{}}),
            };
        }
    }
    Ok(body)
}

fn image_content(images: &crate::InputImages) -> impl Iterator<Item = Value> + '_ {
    images.items().iter().map(|image| json!({"image":{
        "format":match image.media() { crate::ImageMedia::Png => "png", crate::ImageMedia::Jpeg => "jpeg", crate::ImageMedia::Gif => "gif", crate::ImageMedia::Webp => "webp" },
        "source":{"bytes":image.encoded()}
    }}))
}

use crate::transport::sink::DecodeSink;
use crate::{FinishReason, ProviderFailureClass, TokenUsage, ToolExecution};

/// messageStop precedes metadata; settle only after usage has arrived.
#[derive(Debug)]
pub(crate) struct BedrockDecoder {
    binding: crate::replay::ReplayBinding,
    started: bool,
    failed: bool,
    finish: Option<FinishReason>,
    usage: Option<TokenUsage>,
    metadata: bool,
    tools: std::collections::BTreeSet<String>,
    reasoning: Option<(String, crate::replay::bedrock::Accumulation)>,
}
impl BedrockDecoder {
    pub(crate) fn with_binding(binding: crate::replay::ReplayBinding) -> Self {
        Self {
            binding,
            started: false,
            failed: false,
            finish: None,
            usage: None,
            metadata: false,
            tools: Default::default(),
            reasoning: None,
        }
    }
    fn observe_usage(&mut self, value: &Value, sink: &mut DecodeSink<'_>) {
        self.usage = match crate::usage::normalize_bedrock_usage(value) {
            Ok(usage) => usage,
            Err(error) => {
                sink.invalid_usage(error);
                None
            }
        };
    }

    pub(crate) fn decode(&mut self, payload: &str, sink: &mut DecodeSink<'_>) {
        if self.failed {
            return;
        }
        if let Err(reason) = self.event(payload, sink) {
            self.failed = true;
            sink.fail(ProviderFailureClass::InvalidProviderOutput, reason);
        }
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn event(&mut self, payload: &str, sink: &mut DecodeSink<'_>) -> Result<(), &'static str> {
        let value: Value =
            serde_json::from_str(payload).map_err(|_| "invalid Bedrock event JSON")?;
        let event = value
            .as_object()
            .filter(|event| event.len() == 1)
            .ok_or("invalid Bedrock event envelope")?;
        let (kind, value) = event.iter().next().ok_or("empty Bedrock event")?;
        match kind.as_str() {
            "messageStart" => {
                if self.started || value["role"] != "assistant" {
                    return Err("invalid Bedrock message start");
                }
                self.started = true;
            }
            "contentBlockStart" => {
                self.require_started()?;
                let index = index(value)?;
                if let Some(tool) = value["start"].get("toolUse") {
                    if self.reasoning.is_some()
                        || self.tools.len() >= 16
                        || !self.tools.insert(index.clone())
                    {
                        return Err("invalid Bedrock tool block");
                    }
                    let call = tool["toolUseId"]
                        .as_str()
                        .filter(|s| !s.is_empty() && s.len() <= 256)
                        .and_then(grapt_types::ToolCallId::new)
                        .ok_or("invalid Bedrock tool identity")?;
                    let name = tool["name"]
                        .as_str()
                        .filter(|s| !s.is_empty() && s.len() <= 256)
                        .ok_or("invalid Bedrock tool name")?;
                    sink.tool_input_opened(&index, call, name, ToolExecution::Local);
                }
            }
            "contentBlockDelta" => {
                self.require_started()?;
                let index = index(value)?;
                let delta = &value["delta"];
                if delta.as_object().is_none_or(|delta| delta.len() != 1) {
                    return Err("Bedrock content delta must have one member");
                }
                if let Some(reasoning) = delta.get("reasoningContent") {
                    if !self.tools.is_empty() {
                        return Err("Bedrock reasoning overlaps tool input");
                    }
                    sink.close_text();
                    let (held, accumulation) = self
                        .reasoning
                        .get_or_insert_with(|| (index.clone(), Default::default()));
                    if *held != index {
                        return Err("Bedrock reasoning blocks overlap");
                    }
                    if let Some(text) = accumulation.delta(reasoning)? {
                        sink.reasoning_delta(text);
                    }
                } else if self.reasoning.is_some() {
                    return Err("Bedrock content overlaps reasoning");
                } else if let Some(text) = delta["text"].as_str() {
                    sink.text_delta(text);
                } else if let Some(input) = delta["toolUse"]["input"].as_str() {
                    if !self.tools.contains(&index) {
                        return Err("Bedrock tool delta has no opening");
                    }
                    sink.tool_input_delta(&index, input);
                } else {
                    sink.unmodeled();
                }
            }
            "contentBlockStop" => {
                self.require_started()?;
                let index = index(value)?;
                if let Some((held, _)) = &self.reasoning {
                    if *held != index {
                        return Err("Bedrock reasoning stop has another index");
                    }
                    let (_, accumulation) =
                        self.reasoning.take().ok_or("missing Bedrock reasoning")?;
                    let state = accumulation.finish(&self.binding)?;
                    sink.reasoning_delta("");
                    sink.close_reasoning_with_state(state);
                } else if self.tools.remove(&index) {
                    sink.tool_input_final(&index, None);
                    sink.tool_input_closed(&index);
                } else {
                    sink.close_text();
                    sink.close_reasoning();
                }
            }
            "messageStop" => {
                self.require_started()?;
                if self.finish.is_some() || !self.tools.is_empty() || self.reasoning.is_some() {
                    return Err("invalid Bedrock message stop");
                }
                self.finish = Some(match value["stopReason"].as_str() {
                    Some("end_turn" | "stop_sequence") => FinishReason::Stop,
                    Some("tool_use") => FinishReason::ToolCalls,
                    Some("max_tokens" | "model_context_window_exceeded") => FinishReason::Length,
                    Some("content_filtered" | "guardrail_intervened") => {
                        FinishReason::ContentFilter
                    }
                    Some("malformed_model_output" | "malformed_tool_use") => {
                        return Err("Bedrock reported malformed model output")
                    }
                    _ => return Err("unknown Bedrock stop reason"),
                });
            }
            "metadata" => {
                if self.finish.is_none() || self.metadata {
                    return Err("Bedrock metadata is out of order");
                }
                self.observe_usage(value, sink);
                self.metadata = true;
                if let Some(finish) = self.finish {
                    sink.terminal(finish, self.usage.take());
                }
            }
            "exception" => {
                self.failed = true;
                let class = match value["type"].as_str() {
                    Some("throttlingException") => {
                        ProviderFailureClass::RateLimit { retry_after: None }
                    }
                    Some(
                        "serviceUnavailableException"
                        | "internalServerException"
                        | "modelStreamErrorException",
                    ) => ProviderFailureClass::ProviderInternal { retry_after: None },
                    Some("validationException") => ProviderFailureClass::InvalidRequest,
                    Some("accessDeniedException") => ProviderFailureClass::Authentication,
                    _ => ProviderFailureClass::Transport,
                };
                sink.fail(class, "Bedrock returned a stream exception");
            }
            _ => sink.unmodeled(),
        }
        Ok(())
    }
    fn require_started(&self) -> Result<(), &'static str> {
        if self.started && self.finish.is_none() {
            Ok(())
        } else {
            Err("Bedrock event is out of order")
        }
    }
}
fn index(value: &Value) -> Result<String, &'static str> {
    value["contentBlockIndex"]
        .as_u64()
        .filter(|v| *v < 4096)
        .map(|v| v.to_string())
        .ok_or("invalid Bedrock content block index")
}
