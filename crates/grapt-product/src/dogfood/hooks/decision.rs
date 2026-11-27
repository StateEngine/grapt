//! Bounded command decisions, applied only at the existing tool authorization seam.
//! Grapt also supports ask.
//! Copyright 2025 OpenAI. SPDX-License-Identifier: Apache-2.0
//! Licensed under the Apache License, Version 2.0; see LICENSE-APACHE.
//! Provided AS IS, without warranties or conditions of any kind.
use serde::Deserialize;
use serde_json::Value;

#[path = "legacy.rs"]
mod legacy;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::dogfood) enum PermissionDecision {
    #[default]
    Default,
    Allow,
    Ask,
}

#[derive(Debug, Default)]
pub(in crate::dogfood) struct HookResult {
    pub notes: Vec<String>,
    pub permission: PermissionDecision,
    pub input: Option<Value>,
    pub event: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Output {
    decision: Option<String>,
    reason: Option<String>,
    r#continue: Option<bool>,
    stop_reason: Option<String>,
    hook_specific_output: Option<SpecificOutput>,
    updated_input: Option<Value>,
    #[serde(rename = "suppressOutput")]
    _suppress_output: Option<bool>,
    system_message: Option<String>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SpecificOutput {
    hook_event_name: Option<String>,
    decision: Option<RequestDecision>,
    permission_decision: Option<String>,
    permission_decision_reason: Option<String>,
    additional_context: Option<String>,
    updated_input: Option<Value>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}

/// Claude's PermissionRequest uses a nested behavior rather than PreToolUse's
/// permissionDecision. Input and remembered-permission changes remain unsupported.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestDecision {
    behavior: String,
    message: Option<String>,
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn parse(stdout: &str, controls: bool) -> Result<HookResult, String> {
    if !controls {
        return Ok(HookResult {
            notes: legacy::notes(stdout)?,
            ..HookResult::default()
        });
    }
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(HookResult::default());
    }
    if !text.starts_with(['{', '[']) {
        return Ok(HookResult {
            notes: vec![text.into()],
            ..HookResult::default()
        });
    }
    let output: Output =
        serde_json::from_str(text).map_err(|_| "hook returned invalid structured output")?;
    if output.r#continue == Some(false) {
        return Err(format!(
            "hook stopped processing: {}",
            nonempty(output.stop_reason.as_deref())
        ));
    }
    let specific = output.hook_specific_output.unwrap_or_default();
    if controls && (!output.extra.is_empty() || !specific.extra.is_empty()) {
        return Err("hook returned an unknown structured output field".into());
    }
    if matches!(output.decision.as_deref(), Some("deny" | "block"))
        || specific.permission_decision.as_deref() == Some("deny")
    {
        return Err(format!(
            "hook denied: {}",
            nonempty(
                specific
                    .permission_decision_reason
                    .as_deref()
                    .or(output.reason.as_deref())
            )
        ));
    }
    let mut permission = PermissionDecision::Default;
    if let Some(decision) = specific.decision {
        if specific.hook_event_name.as_deref() != Some("PermissionRequest")
            || specific.permission_decision.is_some()
            || output.decision.is_some()
            || output.updated_input.is_some()
            || specific.updated_input.is_some()
        {
            return Err(
                "PermissionRequest hook returned conflicting or unsupported controls".into(),
            );
        }
        match decision.behavior.as_str() {
            "allow" => permission = PermissionDecision::Allow,
            "deny" => {
                return Err(format!(
                    "hook denied: {}",
                    nonempty(decision.message.as_deref())
                ))
            }
            _ => return Err("PermissionRequest behavior must be allow or deny".into()),
        }
    }
    if specific.hook_event_name.as_deref() == Some("PermissionRequest")
        && (specific.permission_decision.is_some()
            || output.decision.is_some()
            || output.updated_input.is_some()
            || specific.updated_input.is_some())
    {
        return Err("PermissionRequest requires decision.behavior and cannot replace input".into());
    }
    let mut modify = false;
    for decision in [
        output.decision.as_deref(),
        specific.permission_decision.as_deref(),
    ] {
        match decision {
            None => {}
            Some("allow" | "approve") => permission = permission.max(PermissionDecision::Allow),
            Some("ask") if controls => permission = PermissionDecision::Ask,
            Some("modify") if controls => modify = true,
            Some(_) => return Err("hook returned an unsupported decision".into()),
        }
    }
    let input = if !controls {
        None
    } else {
        match (output.updated_input, specific.updated_input) {
            (Some(_), Some(_)) => {
                return Err("hook returned conflicting updatedInput fields".into())
            }
            (first, second) => first.or(second),
        }
    };
    if modify && input.is_none() {
        return Err("hook modify decision requires updatedInput".into());
    }
    if input.as_ref().is_some_and(|input| !input.is_object()) {
        return Err("hook updatedInput must be an object".into());
    }
    Ok(HookResult {
        notes: specific
            .additional_context
            .into_iter()
            .chain(output.system_message.filter(|_| controls))
            .filter(|text| !text.trim().is_empty())
            .collect(),
        permission,
        input,
        event: specific.hook_event_name,
    })
}

fn nonempty(text: Option<&str>) -> &str {
    text.map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("no reason supplied")
}
