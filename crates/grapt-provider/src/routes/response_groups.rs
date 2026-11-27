//! Validate explicit request-local response boundaries before any transport.

use crate::{CodexInputItem, MessageRole};
use std::collections::BTreeSet;

pub(super) fn validate(items: &[CodexInputItem]) -> Result<(), &'static str> {
    if !items.iter().any(|item| {
        matches!(
            item,
            CodexInputItem::AssistantResponseStart | CodexInputItem::CompatibleReasoning(_)
        )
    }) {
        return Ok(());
    }
    let mut active = false;
    let mut results = false;
    let mut pending = BTreeSet::new();
    let mut calls = BTreeSet::new();
    for item in items {
        match item {
            CodexInputItem::AssistantResponseStart => {
                if !pending.is_empty() {
                    return Err("assistant response has incomplete tool results");
                }
                active = true;
                results = false;
            }
            CodexInputItem::ToolCall { call, .. } | CodexInputItem::TextToolCall { call, .. }
                if active && !results =>
            {
                if !calls.insert(call) {
                    return Err("assistant response repeats a tool call");
                }
                pending.insert(call);
            }
            CodexInputItem::ToolOutput { call, .. }
            | CodexInputItem::ImageToolOutput { call, .. }
                if active =>
            {
                if !pending.remove(call) {
                    return Err("assistant result has no matching response call");
                }
                results = true;
            }
            CodexInputItem::Message(message)
                if message.role == MessageRole::Assistant && active =>
            {
                if results {
                    return Err("assistant content interrupts its tool results");
                }
            }
            CodexInputItem::ProviderReplay(_) | CodexInputItem::CompatibleReasoning(_)
                if active =>
            {
                if results {
                    return Err("assistant replay interrupts its tool results");
                }
            }
            CodexInputItem::CompatibleReasoning(_)
            | CodexInputItem::ToolCall { .. }
            | CodexInputItem::TextToolCall { .. }
            | CodexInputItem::ToolOutput { .. }
            | CodexInputItem::ImageToolOutput { .. } => {
                return Err("tool exchange is outside its assistant response group");
            }
            _ => {
                if !pending.is_empty() {
                    return Err("assistant response has incomplete tool results");
                }
                active = false;
            }
        }
    }
    if !pending.is_empty() {
        return Err("assistant response has incomplete tool results");
    }
    Ok(())
}
