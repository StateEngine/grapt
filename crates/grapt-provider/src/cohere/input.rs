//! Typed conversation projection to Cohere's native messages and tool exchanges.
// Copyright 2023 Vercel, Inc. SPDX-License-Identifier: Apache-2.0
// Adapted from vercel/ai packages/cohere/src/convert-to-cohere-chat-prompt.ts
// at 7772d0ca0f7abc2cb2a0ad61b0272a72655d780d. Modified for typed inputs.
use crate::{CodexInputItem as Item, InputImages, MessageRole};
use serde_json::{json, Value};

fn images(text: &str, media: &InputImages) -> Value {
    let mut content = vec![json!({"type":"text","text":text})];
    content.extend(media.items().iter().map(|image| json!({
        "type":"image_url", "image_url":{"url":format!("data:{};base64,{}",image.media().mime(),image.encoded())},
    })));
    json!({"role":"user","content":content})
}

fn assistant<'a>(
    messages: &'a mut Vec<Value>,
    boundary: &mut bool,
) -> Result<&'a mut Value, &'static str> {
    if *boundary
        || messages
            .last()
            .is_none_or(|last| last["role"] != "assistant")
    {
        messages.push(json!({"role":"assistant"}));
    }
    *boundary = false;
    messages
        .last_mut()
        .ok_or("Cohere assistant opening is absent")
}

pub(super) fn project(items: &[Item]) -> Result<Vec<Value>, &'static str> {
    let mut messages: Vec<Value> = Vec::new();
    let mut result_images = Vec::new();
    let mut boundary = false;
    for item in items {
        // Finish the parallel tool-result group before adding its user image content.
        if !matches!(item, Item::ToolOutput { .. } | Item::ImageToolOutput { .. }) {
            messages.append(&mut result_images);
        }
        match item {
            Item::AssistantResponseStart => boundary = true,
            Item::Message(message) if message.role == MessageRole::Assistant => {
                let last = assistant(&mut messages, &mut boundary)?;
                let field = if last.get("tool_calls").is_some() {
                    "tool_plan"
                } else {
                    "content"
                };
                let mut text = last[field].as_str().map_or_else(String::new, str::to_owned);
                text.push_str(&message.content);
                last[field] = json!(text);
            }
            Item::Message(message) => {
                let role = match message.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                };
                messages.push(json!({"role":role,"content":message.content}));
                boundary = false;
            }
            Item::ImageUser {
                text,
                images: media,
            } => {
                messages.push(images(text.as_str(), media));
                boundary = false;
            }
            Item::ToolCall {
                call,
                tool,
                arguments,
            }
            | Item::TextToolCall {
                call,
                tool,
                input: arguments,
            } => {
                let args: Value = if matches!(item, Item::TextToolCall { .. }) {
                    json!({"input":arguments.as_str()})
                } else {
                    arguments.object_value()
                };
                let tool = json!({"id":call.as_str(),"type":"function","function":{"name":super::wire_name(tool.as_str()),"arguments":args.to_string()}});
                let last = assistant(&mut messages, &mut boundary)?;
                if let Some(text) = last
                    .as_object_mut()
                    .and_then(|fields| fields.remove("content"))
                {
                    last["tool_plan"] = text;
                }
                if last.get("tool_calls").is_none() {
                    last["tool_calls"] = json!([]);
                }
                last["tool_calls"]
                    .as_array_mut()
                    .ok_or("Cohere tool calls are malformed")?
                    .push(tool);
            }
            Item::ToolOutput { call, output, .. } => {
                messages.push(
                    json!({"role":"tool","tool_call_id":call.as_str(),"content":output.as_str()}),
                );
                boundary = false;
            }
            Item::ImageToolOutput {
                call,
                output,
                images: media,
            } => {
                messages.push(
                    json!({"role":"tool","tool_call_id":call.as_str(),"content":output.as_str()}),
                );
                result_images.push(images("Tool result image:", media));
                boundary = false;
            }
            Item::ProviderReplay(_) => {
                return Err("Cohere cannot replay a foreign provider artifact")
            }
            Item::EffortUpdate(_) | Item::CompactionTrigger | Item::CompatibleReasoning(_) => {
                return Err("Cohere does not accept this input control")
            }
        }
    }
    messages.append(&mut result_images);
    Ok(messages)
}
