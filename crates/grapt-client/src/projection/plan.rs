//! Checkbox presentation for plan replay.
//! Copyright 2025 OpenAI.
//! Licensed under Apache-2.0; see licenses/openai-codex.
//! Modified by Grapt: bounded replay projection and escaped terminal text.
//! Modified by Grapt: bounded validation and existing governed history integration.
use crate::wire::{DurableTool, DurableToolState};
use serde::Deserialize;

#[derive(Deserialize)]
struct Plan {
    explanation: Option<String>,
    plan: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    step: String,
    status: Status,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    InProgress,
    Completed,
}

pub(super) fn present(tool: &DurableTool) -> Option<String> {
    if tool.name != "update_plan" {
        return None;
    }
    let DurableToolState::Completed { input, output } = &tool.state else {
        return None;
    };
    if output != "Plan updated" {
        return None;
    }
    let update: Plan = serde_json::from_str(input).ok()?;
    // A malformed/unsupported result stays visible through the generic tool
    // renderer; it must never masquerade as a completed checklist.
    if update.plan.len() > 64
        || update
            .plan
            .iter()
            .any(|item| item.step.trim().is_empty() || item.step.len() > 1024)
        || update
            .explanation
            .as_ref()
            .is_some_and(|text| text.len() > 4096)
        || update
            .plan
            .iter()
            .filter(|item| item.status == Status::InProgress)
            .count()
            > 1
    {
        return None;
    }
    let completed = update
        .plan
        .iter()
        .filter(|item| item.status == Status::Completed)
        .count();
    let mut text = if update.plan.is_empty() {
        "Plan cleared".to_owned()
    } else {
        format!("Plan updated · {completed}/{} complete", update.plan.len())
    };
    if let Some(explanation) = update.explanation.filter(|text| !text.trim().is_empty()) {
        text.push_str(&format!("\n  {}", display(&explanation)));
    }
    for item in update.plan {
        let marker = match item.status {
            Status::Completed => "[x]",
            Status::InProgress => "[>]",
            Status::Pending => "[ ]",
        };
        text.push_str(&format!("\n  {marker} {}", display(&item.step)));
    }
    Some(text)
}

fn display(text: &str) -> String {
    let flattened = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut shown = String::with_capacity(flattened.len());
    for ch in flattened.chars() {
        if ch.is_control() {
            shown.extend(ch.escape_debug());
        } else {
            shown.push(ch);
        }
    }
    shown
}

#[cfg(test)]
mod tests;
