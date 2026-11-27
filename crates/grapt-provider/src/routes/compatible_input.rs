//! Chat projects one assistant response to one message, then its tool results.

use crate::{CodexInputItem, MessageRole};
use serde_json::{json, Value};

#[cfg(test)]
pub(super) fn project(input: &[CodexInputItem]) -> Result<Vec<Value>, &'static str> {
    project_with_reasoning(input, crate::CompatibleReasoningReplay::None)
}

pub(super) fn project_with_reasoning(
    input: &[CodexInputItem],
    reasoning: crate::CompatibleReasoningReplay,
) -> Result<Vec<Value>, &'static str> {
    let mut messages: Vec<Value> = Vec::new();
    let mut assistant = None;
    // Chat tool messages accept text only. Emit images after the complete
    // result group, retaining their call association in a visible label.
    let mut result_images = Vec::new();
    for item in input {
        if !matches!(
            item,
            CodexInputItem::ToolOutput { .. } | CodexInputItem::ImageToolOutput { .. }
        ) && !result_images.is_empty()
        {
            messages.push(json!({"role":"user","content":std::mem::take(&mut result_images)}));
            assistant = None;
        }
        match item {
            CodexInputItem::CompatibleReasoning(text) => {
                if reasoning == crate::CompatibleReasoningReplay::ReasoningContent {
                    append_reasoning(
                        assistant_message(&mut messages, &mut assistant),
                        text.as_str(),
                    )?;
                }
            }
            CodexInputItem::EffortUpdate(_) => {
                return Err("effort updates require a native protocol")
            }
            CodexInputItem::AssistantResponseStart => assistant = None,
            CodexInputItem::ProviderReplay(_) => {
                return Err("signed replay requires its original protocol")
            }
            CodexInputItem::CompactionTrigger => {
                return Err("a compaction trigger requires the OpenAI Responses protocol")
            }
            CodexInputItem::Message(message) if message.role == MessageRole::Assistant => {
                let projected = assistant_message(&mut messages, &mut assistant);
                let mut text = projected["content"].as_str().unwrap_or_default().to_owned();
                text.push_str(&message.content);
                projected["content"] = Value::String(text);
            }
            CodexInputItem::Message(message) => {
                assistant = None;
                messages.push(json!({"role": match message.role {
                    MessageRole::System => "system", MessageRole::User => "user",
                    MessageRole::Assistant => return Err("unexpected assistant role"),
                }, "content":message.content}));
            }
            CodexInputItem::ImageUser { text, images } => {
                assistant = None;
                messages.push(json!({"role":"user",
                    "content":super::image_content::compatible(text.as_str(), images)}));
            }
            CodexInputItem::ImageToolOutput {
                call,
                output,
                images,
            } => {
                assistant = None;
                messages.push(json!({"role":"tool","tool_call_id":call.as_str(),
                    "content":output.as_str()}));
                result_images.extend(super::image_content::compatible(
                    &format!("Images returned by tool call {}.", call.as_str()),
                    images,
                ));
            }
            CodexInputItem::ToolCall {
                call,
                tool,
                arguments,
            }
            | CodexInputItem::TextToolCall {
                call,
                tool,
                input: arguments,
            } => {
                let message = assistant_message(&mut messages, &mut assistant);
                let calls = message
                    .as_object_mut()
                    .ok_or("assistant projection is not an object")?
                    .entry("tool_calls")
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .ok_or("assistant calls are not an array")?;
                let arguments = if matches!(item, CodexInputItem::TextToolCall { .. }) {
                    json!({"input":arguments.as_str()}).to_string()
                } else {
                    arguments.object_text().to_owned()
                };
                calls.push(json!({"id":call.as_str(),"type":"function",
                    "function":{"name":tool.as_str(),"arguments":arguments}}));
            }
            CodexInputItem::ToolOutput { call, output, .. } => {
                assistant = None;
                messages.push(json!({"role":"tool","tool_call_id":call.as_str(),
                    "content":output.as_str()}));
            }
        }
    }
    if !result_images.is_empty() {
        messages.push(json!({"role":"user","content":result_images}));
    }
    Ok(messages)
}

fn assistant_message<'a>(
    messages: &'a mut Vec<Value>,
    assistant: &mut Option<usize>,
) -> &'a mut Value {
    let index = match *assistant {
        Some(index) => index,
        None => {
            let index = messages.len();
            messages.push(json!({"role":"assistant"}));
            *assistant = Some(index);
            index
        }
    };
    &mut messages[index]
}

fn append_reasoning(message: &mut Value, text: &str) -> Result<(), &'static str> {
    let field = message
        .as_object_mut()
        .ok_or("assistant projection is not an object")?
        .entry("reasoning_content")
        .or_insert_with(|| Value::String(String::new()));
    let Value::String(combined) = field else {
        return Err("assistant reasoning is not text");
    };
    if combined.len().saturating_add(text.len()) > crate::MAX_CODEX_TOOL_TEXT_BYTES {
        return Err("compatible reasoning exceeds its continuation bound");
    }
    combined.push_str(text);
    Ok(())
}
