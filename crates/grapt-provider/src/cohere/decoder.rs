//! Cohere V2's typed SSE events, never a chat-completions decoder.
// Copyright 2023 Vercel, Inc. SPDX-License-Identifier: Apache-2.0
// Adapted from vercel/ai packages/cohere/src/cohere-chat-language-model.ts at
// 7772d0ca0f7abc2cb2a0ad61b0272a72655d780d. Modified for bounded shared sink.
use crate::transport::sink::{DecodeSink, MAX_TOOL_INPUT_BYTES};
use crate::{FinishReason, ProviderFailureClass as Failure, ToolExecution};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Decoder {
    started: bool,
    content: Option<(u32, bool)>,
    tool: Option<(Option<u32>, String, String)>,
    calls: BTreeSet<String>,
    names: BTreeMap<String, String>,
}
impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CohereDecoder").finish_non_exhaustive()
    }
}
impl Decoder {
    pub(crate) fn new(tools: &[crate::ToolDefinition], choice: Option<&crate::ToolChoice>) -> Self {
        Self {
            started: false,
            content: None,
            tool: None,
            calls: BTreeSet::new(),
            names: tools
                .iter()
                .filter(|tool| match choice {
                    Some(crate::ToolChoice::None) => false,
                    Some(crate::ToolChoice::Named { tool: selected }) => selected == &tool.name,
                    _ => true,
                })
                .map(|t| (super::wire_name(&t.name), t.name.clone()))
                .collect(),
        }
    }
    pub(crate) fn decode(&mut self, payload: &str, sink: &mut DecodeSink<'_>) {
        let value: Value = match serde_json::from_str(payload) {
            Ok(value) => value,
            Err(_) => {
                sink.fail(
                    Failure::InvalidProviderOutput,
                    "Cohere sent an invalid JSON frame",
                );
                return;
            }
        };
        if let Err(reason) = self.event(&value, sink) {
            sink.fail(Failure::InvalidProviderOutput, reason);
        }
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn event(&mut self, value: &Value, sink: &mut DecodeSink<'_>) -> Result<(), &'static str> {
        let kind = value["type"]
            .as_str()
            .ok_or("Cohere event type is absent")?;
        if kind == "message-start" {
            if self.started {
                return Err("Cohere repeated message-start");
            }
            self.started = true;
            return Ok(());
        }
        if kind == "error" {
            sink.fail(
                Failure::ProviderInternal { retry_after: None },
                "Cohere returned an error event",
            );
            return Ok(());
        }
        if !self.started {
            return Err("Cohere output preceded message-start");
        }
        let delta = &value["delta"]["message"];
        match kind {
            "content-start" => {
                if self.content.is_some() || self.tool.is_some() {
                    return Err("Cohere content blocks overlap");
                }
                sink.close_reasoning();
                let index = index(value)?.ok_or("Cohere content index is absent")?;
                let content = &delta["content"];
                let thinking = match content["type"].as_str() {
                    Some("text") => false,
                    Some("thinking") => true,
                    _ => return Err("Cohere content type is unsupported"),
                };
                self.content = Some((index, thinking));
                let text = content[if thinking { "thinking" } else { "text" }]
                    .as_str()
                    .ok_or("Cohere content is malformed")?;
                if thinking {
                    sink.reasoning_delta(text);
                } else {
                    sink.text_delta(text);
                }
            }
            "content-delta" => {
                let (held, thinking) = self.content.ok_or("Cohere delta has no content opening")?;
                if index(value)? != Some(held) {
                    return Err("Cohere content delta changed index");
                }
                let text = delta["content"][if thinking { "thinking" } else { "text" }]
                    .as_str()
                    .ok_or("Cohere content delta is malformed")?;
                if thinking {
                    sink.reasoning_delta(text);
                } else {
                    sink.text_delta(text);
                }
            }
            "content-end" => {
                let (held, thinking) = self
                    .content
                    .take()
                    .ok_or("Cohere close has no content opening")?;
                if index(value)? != Some(held) {
                    return Err("Cohere content close changed index");
                }
                if thinking {
                    sink.close_reasoning();
                } else {
                    sink.close_text();
                }
            }
            "tool-plan-delta" => {
                if self.content.is_some() || self.tool.is_some() {
                    return Err("Cohere tool plan overlaps content");
                }
                sink.reasoning_delta(
                    delta["tool_plan"]
                        .as_str()
                        .ok_or("Cohere tool plan is malformed")?,
                );
            }
            "tool-call-start" => {
                if self.tool.is_some() || self.content.is_some() || self.calls.len() >= 16 {
                    return Err("Cohere tool opening exceeds its bound or overlaps content");
                }
                sink.close_reasoning();
                let call = &delta["tool_calls"];
                if call["type"] != "function" {
                    return Err("Cohere tool call type is unsupported");
                }
                let id = call["id"]
                    .as_str()
                    .filter(|id| id.len() <= 256)
                    .ok_or("Cohere call identity is malformed")?;
                let typed =
                    grapt_types::ToolCallId::new(id).ok_or("Cohere call identity is invalid")?;
                if !self.calls.insert(id.to_owned()) {
                    return Err("Cohere repeated a tool identity");
                }
                let name = call["function"]["name"]
                    .as_str()
                    .and_then(|name| self.names.get(name))
                    .ok_or("Cohere called an unoffered function")?;
                let arguments = call["function"]["arguments"]
                    .as_str()
                    .filter(|text| text.len() <= MAX_TOOL_INPUT_BYTES)
                    .ok_or("Cohere initial tool arguments are malformed")?;
                self.tool = Some((index(value)?, id.to_owned(), arguments.to_owned()));
                sink.tool_input_opened(id, typed, name, ToolExecution::Local);
            }
            "tool-call-delta" => {
                let (held, _, arguments) = self
                    .tool
                    .as_mut()
                    .ok_or("Cohere tool delta has no opening")?;
                check_index(*held, index(value)?)?;
                let text = delta["tool_calls"]["function"]["arguments"]
                    .as_str()
                    .ok_or("Cohere tool delta is malformed")?;
                if text.len() > MAX_TOOL_INPUT_BYTES.saturating_sub(arguments.len()) {
                    return Err("Cohere tool arguments exceed their byte bound");
                }
                arguments.push_str(text);
            }
            "tool-call-end" => {
                let (held, id, arguments) =
                    self.tool.take().ok_or("Cohere tool close has no opening")?;
                check_index(held, index(value)?)?;
                let arguments = if arguments.trim().is_empty() {
                    "{}"
                } else {
                    arguments.as_str()
                };
                let parsed: Value = serde_json::from_str(arguments)
                    .map_err(|_| "Cohere tool arguments are not JSON")?;
                if !parsed.is_object() && !parsed.is_null() {
                    return Err("Cohere tool arguments must be an object");
                }
                // Cohere uses null or empty arguments for parameterless functions.
                sink.tool_input_delta(&id, if parsed.is_null() { "{}" } else { arguments });
                sink.tool_input_closed(&id);
            }
            "message-end" => {
                if self.tool.is_some() || self.content.is_some() {
                    return Err("Cohere finished with an open block");
                }
                sink.close_reasoning();
                let usage = match crate::usage::normalize_cohere_usage(&value["delta"]) {
                    Ok(value) => value,
                    Err(error) => {
                        sink.invalid_usage(error);
                        None
                    }
                };
                let finish = match value["delta"]["finish_reason"].as_str() {
                    Some("COMPLETE" | "STOP_SEQUENCE") if !self.calls.is_empty() => {
                        FinishReason::ToolCalls
                    }
                    Some("COMPLETE" | "STOP_SEQUENCE") => FinishReason::Stop,
                    Some("TOOL_CALL") if !self.calls.is_empty() => FinishReason::ToolCalls,
                    Some("MAX_TOKENS") => FinishReason::Length,
                    Some("TIMEOUT") => {
                        sink.fail(
                            Failure::ProviderInternal { retry_after: None },
                            "Cohere generation exceeded its time limit",
                        );
                        return Ok(());
                    }
                    Some("ERROR") => {
                        sink.fail(
                            Failure::ProviderInternal { retry_after: None },
                            "Cohere ended with an error",
                        );
                        return Ok(());
                    }
                    _ => return Err("Cohere finish reason is absent or unsupported"),
                };
                sink.terminal(finish, usage);
            }
            // RAG citation metadata and diagnostics are observable as unmodeled events.
            _ => sink.unmodeled(),
        }
        Ok(())
    }
}
fn index(value: &Value) -> Result<Option<u32>, &'static str> {
    value
        .get("index")
        .map(|index| {
            index
                .as_u64()
                .filter(|n| *n < 4096)
                .map(|n| n as u32)
                .ok_or("Cohere event index exceeds its bound")
        })
        .transpose()
}
fn check_index(held: Option<u32>, next: Option<u32>) -> Result<(), &'static str> {
    if held.is_some() && next.is_some() && held != next {
        Err("Cohere tool delta changed index")
    } else {
        Ok(())
    }
}
