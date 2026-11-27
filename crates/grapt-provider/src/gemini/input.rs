//! Project typed conversation items to native model/user contents and function responses.
use crate::{CodexInputItem as Item, InputImages, MessageRole};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn push(contents: &mut Vec<Value>, role: &str, part: Value) {
    if let Some(last) = contents.last_mut().filter(|last| last["role"] == role) {
        if let Some(parts) = last["parts"].as_array_mut() {
            parts.push(part);
        }
    } else {
        contents.push(json!({"role":role,"parts":[part]}));
    }
}
fn model(contents: &mut Vec<Value>, boundary: &mut bool, part: Value) {
    if *boundary && contents.last().is_some_and(|last| last["role"] == "model") {
        contents.push(json!({"role":"model","parts":[]}));
    }
    *boundary = false;
    push(contents, "model", part);
}
fn images(contents: &mut Vec<Value>, images: &InputImages) {
    for image in images.items() {
        push(
            contents,
            "user",
            json!({"inlineData":{"mimeType":image.media().mime(),"data":image.encoded()}}),
        );
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn project(
    input: &[Item],
    selected_model: &str,
) -> Result<(Vec<Value>, Vec<Value>), &'static str> {
    let mut contents = Vec::new();
    let mut system = Vec::new();
    let mut names = BTreeMap::new();
    let mut native_calls = BTreeMap::new();
    let mut boundary = false;
    for item in input {
        if let Item::ProviderReplay(replay) = item {
            if let Some(part) = replay.gemini_part() {
                if let Some(id) = part["functionCall"]["id"].as_str() {
                    if native_calls.insert(id, part).is_some() {
                        return Err("duplicate Gemini call replay");
                    }
                }
            }
        }
    }
    for item in input {
        match item {
            Item::AssistantResponseStart => {
                boundary = true;
            }
            Item::EffortUpdate(_) | Item::CompactionTrigger | Item::CompatibleReasoning(_) => {
                return Err("Gemini does not accept this input control")
            }
            Item::ProviderReplay(replay) => {
                let part = replay
                    .gemini_part()
                    .ok_or("foreign provider replay on Gemini")?;
                // Calls are emitted at their typed ToolCall position, not the artifact position.
                if part.get("functionCall").is_none() {
                    model(&mut contents, &mut boundary, part.clone());
                }
            }
            Item::Message(message) => match message.role {
                MessageRole::System => system.push(json!({"text":message.content})),
                MessageRole::User => push(&mut contents, "user", json!({"text":message.content})),
                MessageRole::Assistant => model(
                    &mut contents,
                    &mut boundary,
                    json!({"text":message.content}),
                ),
            },
            Item::ImageUser {
                text,
                images: media,
            } => {
                push(&mut contents, "user", json!({"text":text.as_str()}));
                images(&mut contents, media);
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
                let typed = if matches!(item, Item::TextToolCall { .. }) {
                    Some(json!({"input":arguments.as_str()}))
                } else {
                    arguments.object()
                };
                let replaced = typed.is_none();
                let args = typed.unwrap_or_else(|| arguments.object_value());
                let function =
                    json!({"id":call.as_str(),"name":super::wire_name(tool.as_str()),"args":args});
                let part = if let Some(native) = native_calls.get(call.as_str()) {
                    // The native part carries the provider's own arguments. When
                    // the typed arguments had to be replaced, only the call's
                    // identity can agree.
                    let agrees = if replaced {
                        native["functionCall"]["id"] == function["id"]
                            && native["functionCall"]["name"] == function["name"]
                    } else {
                        native["functionCall"] == function
                    };
                    if !agrees {
                        return Err("Gemini native and typed call disagree");
                    }
                    (*native).clone()
                } else {
                    let mut part = json!({"functionCall":function});
                    // Google documents this sentinel for imported/cross-model tool history
                    // whose original signature is unavailable; live signatures are retained.
                    if selected_model.starts_with("gemini-3") {
                        part["thoughtSignature"] = json!("skip_thought_signature_validator");
                    }
                    part
                };
                if names
                    .insert(call.as_str(), super::wire_name(tool.as_str()))
                    .is_some()
                {
                    return Err("duplicate Gemini tool call identity");
                }
                model(&mut contents, &mut boundary, part);
            }
            Item::ToolOutput {
                call,
                output,
                is_error,
            } => {
                let name = names
                    .get(call.as_str())
                    .ok_or("Gemini result has no call")?;
                let response = if *is_error {
                    json!({"error":output.as_str()})
                } else {
                    json!({"output":output.as_str()})
                };
                push(
                    &mut contents,
                    "user",
                    json!({"functionResponse":{"id":call.as_str(),"name":name,"response":response}}),
                );
            }
            Item::ImageToolOutput {
                call,
                output,
                images: media,
            } => {
                let name = names
                    .get(call.as_str())
                    .ok_or("Gemini image result has no call")?;
                push(
                    &mut contents,
                    "user",
                    json!({"functionResponse":{"id":call.as_str(),"name":name,"response":{"output":output.as_str()}}}),
                );
                images(&mut contents, media);
            }
        }
    }
    Ok((contents, system))
}
