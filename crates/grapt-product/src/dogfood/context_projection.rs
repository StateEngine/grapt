//! A bounded request view that preserves governing context and live tool pairs.
use crate::dogfood::continuation::admission::{input, input_mut};
use grapt_provider::{CodexInputItem, InputMessage, MessageRole, ProviderRequest};
use std::collections::BTreeSet;

/// Build a request projection without changing durable conversation history.
pub(in crate::dogfood) fn trim(
    request: &mut ProviderRequest,
    history_start: usize,
    protected_from: usize,
    label: &str,
    encoding: &grapt_provider::RequestEncoding,
    limit: u64,
) -> Result<u64, String> {
    // Hooks, mail and the complete live exchange follow the current input.
    // Prefix instructions (including authorization) stay byte-for-byte.
    if protected_from < history_start || protected_from > input(request).len() {
        return Err("context trim has an invalid protected input boundary".into());
    }
    let rows = input(request);
    let mut users: Vec<_> = (history_start..protected_from)
        .filter(|index| is_user(&rows[*index]))
        .collect();
    users.push(protected_from);
    let candidates: Vec<_> = users
        .windows(2)
        .skip(1)
        .filter(|pair| complete_exchange(&rows[pair[0]..pair[1]]))
        .map(|pair| (pair[0], pair[1]))
        .collect();
    // The first retained user item is the original task or authoritative
    // continuation anchor. Keep it, the current input, all system updates,
    // and whole recent exchanges; older completed turns go first.
    input_mut(request).insert(
        history_start,
        CodexInputItem::Message(InputMessage {
            role: MessageRole::System,
            content: label.into(),
        }),
    );
    let mut removed = 0;
    let mut after = encoding.estimate(request)?;
    for (start, end) in candidates {
        if after <= limit {
            break;
        }
        input_mut(request).drain(start + 1 - removed..end + 1 - removed);
        removed += end - start;
        after = encoding.estimate(request)?;
    }
    if after > limit {
        return Err("This request's protected instructions, authorization and current input cannot fit the model context. History is intact; send a shorter prompt or choose a larger context window.".into());
    }
    Ok(after)
}

fn is_user(item: &CodexInputItem) -> bool {
    matches!(
        item,
        CodexInputItem::Message(InputMessage {
            role: MessageRole::User,
            ..
        }) | CodexInputItem::ImageUser { .. }
    )
}
fn complete_exchange(items: &[CodexInputItem]) -> bool {
    let mut calls = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    for item in items {
        match item {
            CodexInputItem::Message(InputMessage {
                role: MessageRole::System,
                ..
            })
            | CodexInputItem::EffortUpdate(_)
            | CodexInputItem::ProviderReplay(_) => return false,
            CodexInputItem::ToolCall { call, .. } | CodexInputItem::TextToolCall { call, .. }
                if !calls.insert(call) =>
            {
                return false;
            }
            CodexInputItem::ToolOutput { call, .. }
            | CodexInputItem::ImageToolOutput { call, .. }
                if !outputs.insert(call) =>
            {
                return false;
            }
            _ => {}
        }
    }
    calls == outputs
        && items
            .iter()
            .any(|item| matches!(item, CodexInputItem::AssistantResponseStart))
}
