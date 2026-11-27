//! GenerateContent SSE chunks, with bounded signed-part replay.
// Copyright 2023 Vercel, Inc. SPDX-License-Identifier: Apache-2.0
// Adapted from vercel/ai packages/google/src/google-language-model.ts at
// 7772d0ca0f7abc2cb2a0ad61b0272a72655d780d. Modified: shared sink and sealed replay.
use crate::replay::ReplayBinding;
use crate::transport::sink::DecodeSink;
use crate::{FinishReason, ProviderFailureClass as Failure, TokenUsage, ToolExecution};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(crate) struct Decoder {
    binding: ReplayBinding,
    attempt: String,
    names: std::collections::BTreeMap<String, String>,
    calls: BTreeSet<String>,
    text: Option<Value>,
    finish: Option<FinishReason>,
    usage: Option<TokenUsage>,
}
impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiDecoder").finish_non_exhaustive()
    }
}
impl Decoder {
    pub(crate) fn new(
        binding: ReplayBinding,
        attempt: String,
        tools: &[crate::ToolDefinition],
    ) -> Self {
        Self {
            binding,
            attempt,
            names: tools
                .iter()
                .map(|tool| (super::wire_name(&tool.name), tool.name.clone()))
                .collect(),
            calls: BTreeSet::new(),
            text: None,
            finish: None,
            usage: None,
        }
    }
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(crate) fn decode(&mut self, payload: &str, sink: &mut DecodeSink<'_>) {
        let value: Value = match serde_json::from_str(payload) {
            Ok(value) => value,
            Err(_) => {
                sink.fail(
                    Failure::InvalidProviderOutput,
                    "Gemini sent an invalid JSON frame",
                );
                return;
            }
        };
        if let Some(error) = value.get("error") {
            let class = match error["status"].as_str() {
                Some("UNAUTHENTICATED" | "PERMISSION_DENIED") => Failure::Authentication,
                Some("RESOURCE_EXHAUSTED") => Failure::RateLimit { retry_after: None },
                Some("INTERNAL" | "UNAVAILABLE") => Failure::ProviderInternal { retry_after: None },
                Some("INVALID_ARGUMENT") => Failure::InvalidRequest,
                Some("NOT_FOUND") => Failure::NoRoute,
                _ => Failure::InvalidProviderOutput,
            };
            sink.fail(class, "Gemini returned an error in the stream");
            return;
        }
        match crate::usage::normalize_gemini_usage(&value) {
            Ok(Some(usage)) => {
                sink.remember_usage(usage.clone());
                self.usage = Some(usage);
            }
            Ok(None) => (),
            Err(error) => {
                sink.invalid_usage(error);
            }
        }
        if value["promptFeedback"].get("blockReason").is_some() {
            sink.terminal(FinishReason::ContentFilter, self.usage.take());
            return;
        }
        let Some(candidates) = value.get("candidates").and_then(Value::as_array) else {
            if value.get("usageMetadata").is_none() {
                sink.fail(
                    Failure::InvalidProviderOutput,
                    "Gemini frame has no candidates or usage",
                );
            }
            return;
        };
        if candidates.len() > 1
            || candidates.first().is_some_and(|candidate| {
                candidate["index"].as_u64().is_some_and(|index| index != 0)
            })
        {
            sink.fail(
                Failure::InvalidProviderOutput,
                "Gemini returned an unexpected candidate",
            );
            return;
        }
        let Some(candidate) = candidates.first() else {
            return;
        };
        if let Some(parts) = candidate["content"].get("parts") {
            let Some(parts) = parts.as_array().filter(|parts| parts.len() <= 1024) else {
                sink.fail(
                    Failure::InvalidProviderOutput,
                    "Gemini parts exceed their shape bound",
                );
                return;
            };
            if self.finish.is_some() && !parts.is_empty() {
                sink.fail(
                    Failure::InvalidProviderOutput,
                    "Gemini output followed its finish",
                );
                return;
            }
            for (index, part) in parts.iter().enumerate() {
                // Distinct parts in one response keep their positional context.
                if index != 0 {
                    if let Err(reason) = self.close_text(sink) {
                        sink.fail(Failure::InvalidProviderOutput, reason);
                        return;
                    }
                }
                if let Err(reason) = self.part(part, sink) {
                    sink.fail(Failure::InvalidProviderOutput, reason);
                    return;
                }
            }
        }
        if let Some(reason) = candidate.get("finishReason") {
            let Some(reason) = reason.as_str() else {
                sink.fail(
                    Failure::InvalidProviderOutput,
                    "Gemini finish reason is invalid",
                );
                return;
            };
            if let Err(reason) = self.close_text(sink) {
                sink.fail(Failure::InvalidProviderOutput, reason);
                return;
            }
            self.finish = Some(match reason {
                "STOP" if !self.calls.is_empty() => FinishReason::ToolCalls,
                "STOP" => FinishReason::Stop,
                "MAX_TOKENS" => FinishReason::Length,
                "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII"
                | "IMAGE_SAFETY" => FinishReason::ContentFilter,
                "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL" => {
                    sink.fail(
                        Failure::InvalidProviderOutput,
                        "Gemini refused its generated function call",
                    );
                    return;
                }
                _ => FinishReason::Unknown,
            });
        }
    }
    fn part(&mut self, raw: &Value, sink: &mut DecodeSink<'_>) -> Result<(), &'static str> {
        if let Some(call) = raw.get("functionCall") {
            if !call.is_object() {
                return Err("Gemini function call is not an object");
            }
            self.close_text(sink)?;
            if self.calls.len() >= 16 {
                return Err("Gemini function call count exceeds its bound");
            }
            let mut call = call.clone();
            let id = match call.get("id") {
                Some(value) => value
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 256)
                    .ok_or("Gemini call identity is malformed")?
                    .to_owned(),
                None => format!("gemini-{}-{}", self.attempt, self.calls.len()),
            };
            if !self.calls.insert(id.clone()) {
                return Err("Gemini repeated a function call identity");
            }
            call["id"] = json!(id);
            if call.get("args").is_none() {
                call["args"] = json!({});
            }
            let mut part = raw.clone();
            part["functionCall"] = call;
            let part = super::replay::part(&part)?;
            let state = super::replay::state(&part, &self.binding)?;
            let call = &part["functionCall"];
            let name = call["name"].as_str().ok_or("Gemini call name is missing")?;
            let name = self
                .names
                .get(name)
                .ok_or("Gemini called an unoffered function")?;
            let id = grapt_types::ToolCallId::new(id).ok_or("Gemini call identity is invalid")?;
            let key = id.as_str().to_owned();
            sink.tool_input_opened(&key, id, name, ToolExecution::Local);
            sink.tool_input_delta(&key, &call["args"].to_string());
            sink.tool_input_closed_with_state(&key, state);
            return Ok(());
        }
        if raw.get("text").is_none() && raw.get("thoughtSignature").is_some() {
            self.close_text(sink)?;
            let state = super::replay::state(raw, &self.binding)?;
            // An empty signed part still owns replay state at this position.
            sink.text_delta("");
            sink.close_text_with_state(state);
            return Ok(());
        }
        let part = super::replay::part(raw)?;
        let thought = part["thought"] == true;
        let text = part["text"].as_str().ok_or("Gemini part has no text")?;
        let merge = self.text.as_ref().is_some_and(|held| {
            (held["thought"] == true) == thought
                && held.get("thoughtSignature").is_none()
                && part.get("thoughtSignature").is_none()
        });
        if !merge {
            self.close_text(sink)?;
            let mut held = part.clone();
            held["text"] = json!("");
            self.text = Some(held);
        }
        let held = self
            .text
            .as_mut()
            .ok_or("Gemini text accumulation is absent")?;
        let mut accumulated = held["text"]
            .as_str()
            .ok_or("Gemini text accumulation is malformed")?
            .to_owned();
        if accumulated.len().saturating_add(text.len()) > 64 * 1024 {
            return Err("Gemini text part exceeds its byte bound");
        }
        accumulated.push_str(text);
        held["text"] = json!(accumulated);
        if let Some(signature) = part.get("thoughtSignature") {
            held["thoughtSignature"] = signature.clone();
        }
        if thought {
            sink.reasoning_delta(text);
        } else {
            sink.text_delta(text);
        }
        Ok(())
    }
    fn close_text(&mut self, sink: &mut DecodeSink<'_>) -> Result<(), &'static str> {
        let Some(part) = self.text.take() else {
            return Ok(());
        };
        let state = if part.get("thoughtSignature").is_some() {
            super::replay::state(&part, &self.binding)?
        } else {
            crate::AdapterState::none()
        };
        if part["thought"] == true {
            sink.close_reasoning_with_state(state);
        } else {
            sink.close_text_with_state(state);
        }
        Ok(())
    }
    pub(crate) fn finish(&mut self, sink: &mut DecodeSink<'_>) {
        if let Some(finish) = self.finish.take() {
            sink.terminal(finish, self.usage.take());
        }
    }
}
