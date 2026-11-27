// Upstream copyright and MIT license: see THIRD_PARTY_NOTICES.md.
// Modified by Grapt: retain System authority, refuse impossible placements,
// and reuse the existing typed tool-call/result validator.
//! Messages projection; tool results retain their exact call association.

use std::collections::BTreeSet;

use crate::{CodexInputItem, MessageRole};
use serde_json::{json, Value};

#[cfg(test)]
pub(super) fn project(
    input: &[CodexInputItem],
    system_updates: bool,
) -> Result<(Vec<&str>, Vec<Value>), &'static str> {
    project_with_effort(
        input,
        system_updates,
        &crate::effort::Projection {
            baseline: None,
            updates: false,
        },
    )
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn project_with_effort<'a>(
    input: &'a [CodexInputItem],
    system_updates: bool,
    effort: &crate::effort::Projection,
) -> Result<(Vec<&'a str>, Vec<Value>), &'static str> {
    let mut established = effort.baseline;
    let mut system = Vec::new();
    let mut updates = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    let mut pending = BTreeSet::new();
    let mut calls = BTreeSet::new();
    let mut results_started = false;
    let mut boundary = false;
    for item in input {
        if let CodexInputItem::EffortUpdate(value) = item {
            if effort.updates && established != Some(*value) {
                if !value.native() || !pending.is_empty() {
                    return Err("invalid native effort update or incomplete tool group");
                }
                // Effort-only System controls are valid before User input and
                // do not obey content-bearing System placement rules.
                messages.push(
                    json!({"role":"system","content":[],"output_config":{"effort":value.as_str()}}),
                );
                boundary = true;
            }
            established = Some(*value);
            continue;
        }
        let (role, block) = match item {
            CodexInputItem::EffortUpdate(_) => return Err("unexpected effort control"),
            CodexInputItem::AssistantResponseStart => {
                if !pending.is_empty() {
                    return Err("Anthropic response starts before its preceding results");
                }
                boundary = true;
                continue;
            }
            CodexInputItem::CompatibleReasoning(_) => {
                return Err("compatible reasoning requires its declared protocol")
            }
            CodexInputItem::CompactionTrigger => {
                return Err("a compaction trigger requires the OpenAI Responses protocol")
            }
            CodexInputItem::ProviderReplay(replay) => {
                if !pending.is_empty() && results_started {
                    return Err("Anthropic replay interrupts unresolved tool results");
                }
                if let Some(call) = replay.native_search_call() {
                    if !calls.insert(call) {
                        return Err("Anthropic history contains a duplicate tool call");
                    }
                }
                (
                    "assistant",
                    replay
                        .anthropic_block()
                        .ok_or("signed replay requires its original protocol")?,
                )
            }
            CodexInputItem::Message(message) => {
                if message.role == MessageRole::System {
                    if messages.is_empty() {
                        system.push(message.content.as_str());
                    } else {
                        if !system_updates {
                            return Err(
                                "Anthropic system instructions must precede conversation history",
                            );
                        }
                        if !pending.is_empty() {
                            return Err("Anthropic system update splits tool calls and results");
                        }
                        updates.push(message.content.as_str());
                    }
                    continue;
                }
                if !pending.is_empty() && (message.role == MessageRole::User || results_started) {
                    return Err("Anthropic tool results must immediately follow their calls");
                }
                (
                    match message.role {
                        MessageRole::Assistant => "assistant",
                        MessageRole::User => "user",
                        MessageRole::System => return Err("unexpected system message"),
                    },
                    json!({"type":"text","text":message.content}),
                )
            }
            CodexInputItem::ImageUser { text, images } => {
                if !pending.is_empty() {
                    return Err("Anthropic image input interrupts unresolved tool results");
                }
                (
                    "user",
                    Value::Array(super::image_content::anthropic(text.as_str(), images)),
                )
            }
            CodexInputItem::ImageToolOutput {
                call,
                output,
                images,
            } => {
                if !pending.remove(call) {
                    return Err("Anthropic image tool result has no pending call");
                }
                results_started = true;
                (
                    "user",
                    json!({"type":"tool_result","tool_use_id":call.as_str(),
                    "content":super::image_content::anthropic(output.as_str(), images),
                    "is_error":false}),
                )
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
                if results_started && !pending.is_empty() {
                    return Err("Anthropic tool result group is incomplete");
                }
                if !calls.insert(call) {
                    return Err("Anthropic history contains a duplicate tool call");
                }
                let arguments: Value = if matches!(item, CodexInputItem::TextToolCall { .. }) {
                    json!({"input":arguments.as_str()})
                } else {
                    arguments.object_value()
                };
                pending.insert(call);
                results_started = false;
                (
                    "assistant",
                    json!({"type":"tool_use","id":call.as_str(),
                    "name":tool.as_str(),"input":arguments}),
                )
            }
            CodexInputItem::ToolOutput {
                call,
                output,
                is_error,
            } => {
                if !pending.remove(call) {
                    return Err("Anthropic tool result has no pending call");
                }
                results_started = true;
                (
                    "user",
                    json!({"type":"tool_result","tool_use_id":call.as_str(),
                    "content":output.as_str(),"is_error":is_error}),
                )
            }
        };
        if role == "assistant" {
            release_updates(&mut messages, &mut updates)?;
        }
        if let Some(previous) = messages
            .last_mut()
            .filter(|previous| !boundary && previous["role"] == role)
        {
            let Some(content) = previous["content"].as_array_mut() else {
                return Err("Anthropic content projection is not an array");
            };
            match block {
                Value::Array(blocks) => content.extend(blocks),
                block => content.push(block),
            }
        } else {
            let blocks = match block {
                Value::Array(blocks) => blocks,
                block => vec![block],
            };
            messages.push(json!({"role":role,"content":blocks}));
        }
        boundary = false;
    }
    if !pending.is_empty() {
        return Err("Anthropic history has unresolved tool calls");
    }
    release_updates(&mut messages, &mut updates)?;
    Ok((system, messages))
}

fn release_updates(messages: &mut Vec<Value>, updates: &mut Vec<&str>) -> Result<(), &'static str> {
    if updates.is_empty() {
        return Ok(());
    }
    // Content-bearing System messages must be between a complete user turn
    // and the next assistant turn, or end the array. Never lower them to User.
    if messages
        .iter()
        .rev()
        .find(|message| message["role"] != "system")
        .is_none_or(|message| message["role"] != "user")
    {
        return Err("Anthropic system update requires a complete user turn");
    }
    for text in updates.drain(..) {
        messages.push(json!({"role":"system","content":[{"type":"text","text":text}]}));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CodexToolText, InputMessage, ToolName};
    use grapt_types::ToolCallId;

    fn exchange() -> Result<Vec<CodexInputItem>, Box<dyn std::error::Error>> {
        let call = ToolCallId::new("call_pc").ok_or("call")?;
        Ok(vec![
            InputMessage {
                role: MessageRole::System,
                content: "PC instructions".into(),
            }
            .into(),
            InputMessage {
                role: MessageRole::User,
                content: "inspect".into(),
            }
            .into(),
            CodexInputItem::ToolCall {
                call: call.clone(),
                tool: ToolName::new("read_file").ok_or("tool")?,
                arguments: CodexToolText::try_new("{\"path\":\"a\"}".into())?,
            },
            CodexInputItem::ToolOutput {
                call,
                output: CodexToolText::try_new("denied".into())?,
                is_error: true,
            },
        ])
    }

    #[test]
    fn pc_anthropic_projects_instructions_and_failed_tool_association(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let input = exchange()?;
        let (system, messages) = project(&input, false)?;
        assert_eq!(system, vec!["PC instructions"]);
        assert_eq!(
            messages,
            vec![
                json!({"role":"user","content":[{"type":"text","text":"inspect"}]}),
                json!({"role":"assistant","content":[{"type":"tool_use","id":"call_pc","name":"read_file","input":{"path":"a"}}]}),
                json!({"role":"user","content":[{"type":"tool_result","tool_use_id":"call_pc","content":"denied","is_error":true}]}),
            ]
        );
        Ok(())
    }

    #[test]
    fn pc_anthropic_refuses_missing_and_misordered_tool_history(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let input = exchange()?;
        assert!(project(&input[..3], false).is_err());
        let mut wrong = input.clone();
        wrong.swap(2, 3);
        assert!(project(&wrong, false).is_err());
        let mut wrong = input.clone();
        wrong.insert(
            3,
            InputMessage {
                role: MessageRole::User,
                content: "interrupt".into(),
            }
            .into(),
        );
        assert!(project(&wrong, false).is_err());
        Ok(())
    }

    #[test]
    fn pc_anthropic_preserves_text_after_tool_use_in_the_same_assistant_message(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut input = exchange()?;
        input.insert(
            3,
            InputMessage {
                role: MessageRole::Assistant,
                content: "checking now".into(),
            }
            .into(),
        );
        let (_, messages) = project(&input, false)?;
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(
            messages[1]["content"][1],
            json!({"type":"text","text":"checking now"})
        );
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "call_pc");
        Ok(())
    }
    fn text(role: MessageRole, content: &str) -> CodexInputItem {
        InputMessage {
            role,
            content: content.into(),
        }
        .into()
    }

    #[test]
    fn system_updates_follow_the_complete_user_turn_and_keep_their_role(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let input = vec![
            text(MessageRole::System, "baseline"),
            text(MessageRole::User, "first"),
            text(MessageRole::Assistant, "answer"),
            text(MessageRole::System, "new policy"),
            text(MessageRole::User, "second"),
            text(MessageRole::User, "more input"),
        ];
        let (system, messages) = project(&input, true)?;
        assert_eq!(system, ["baseline"]);
        assert_eq!(
            messages
                .iter()
                .map(|message| message["role"].as_str())
                .collect::<Vec<_>>(),
            [
                Some("user"),
                Some("assistant"),
                Some("user"),
                Some("system")
            ]
        );
        assert_eq!(
            messages[2]["content"]
                .as_array()
                .ok_or("user content")?
                .len(),
            2
        );
        assert_eq!(messages[3]["content"][0]["text"], "new policy");
        let mut continued = input;
        continued.push(text(MessageRole::Assistant, "next answer"));
        continued.push(text(MessageRole::User, "third"));
        let (_, next) = project(&continued, true)?;
        assert_eq!(next[..messages.len()], messages);
        Ok(())
    }

    #[test]
    fn system_updates_do_not_split_tools_or_become_user_text(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut input = exchange()?;
        input.push(text(MessageRole::System, "updated after results"));
        let (_, messages) = project(&input, true)?;
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[3]["role"], "system");
        assert!(project(&input, false).is_err());
        input.insert(3, text(MessageRole::System, "cannot split"));
        assert!(project(&input, true).is_err());
        let impossible = [
            text(MessageRole::User, "question"),
            text(MessageRole::Assistant, "answer"),
            text(MessageRole::System, "no following user"),
        ];
        assert!(project(&impossible, true).is_err());
        Ok(())
    }
}
