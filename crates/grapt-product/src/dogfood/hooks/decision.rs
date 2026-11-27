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

/// Why a hook produced no decision to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::dogfood) enum HookError {
    /// The hook's own refusal (exit 2, deny, block, or `continue: false`). It
    /// always stands.
    Refused(String),
    /// The hook itself failed. Only a hook the user marked required stops the
    /// turn; any other is skipped with a visible warning.
    Failed(String),
}

impl std::fmt::Display for HookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) | Self::Failed(reason) => f.write_str(reason),
        }
    }
}

impl From<HookError> for String {
    fn from(error: HookError) -> Self {
        error.to_string()
    }
}

impl From<String> for HookError {
    fn from(reason: String) -> Self {
        Self::Failed(reason)
    }
}

impl From<&str> for HookError {
    fn from(reason: &str) -> Self {
        Self::Failed(reason.to_owned())
    }
}

// Refusal and reason fields are read by `explicit_refusal`; the typed shape
// still validates them on every other output.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Output {
    decision: Option<String>,
    #[serde(rename = "reason")]
    _reason: Option<String>,
    #[serde(rename = "continue")]
    _continue: Option<bool>,
    #[serde(rename = "stopReason")]
    _stop_reason: Option<String>,
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
    #[serde(rename = "permissionDecisionReason")]
    _permission_decision_reason: Option<String>,
    additional_context: Option<String>,
    updated_input: Option<Value>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}

/// Claude's PermissionRequest uses a nested behavior rather than PreToolUse's
/// permissionDecision. Input and remembered-permission changes remain unsupported.
#[derive(Deserialize)]
struct RequestDecision {
    behavior: String,
    #[serde(rename = "message")]
    _message: Option<String>,
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, Value>,
}

pub(super) fn parse(stdout: &str, controls: bool) -> Result<HookResult, HookError> {
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
    if let Some(refusal) = explicit_refusal(text, true) {
        return Err(refusal);
    }
    let output: Output =
        serde_json::from_str(text).map_err(|_| "hook returned invalid structured output")?;
    let specific = output.hook_specific_output.unwrap_or_default();
    let nested = specific.decision.as_ref();
    // Checked after any refusal: an extra field never weakens a deny.
    if controls
        && (!output.extra.is_empty()
            || !specific.extra.is_empty()
            || nested.is_some_and(|decision| !decision.extra.is_empty()))
    {
        return Err("hook returned an unknown structured output field".into());
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

/// A refusal the hook stated explicitly, read from the raw JSON before typed
/// decoding so that a malformed reason or other field never weakens it into a
/// skippable failure. `nested` admits the PermissionRequest `decision.behavior`.
pub(super) fn explicit_refusal(text: &str, nested: bool) -> Option<HookError> {
    fn field<'a>(owner: Option<&'a Value>, key: &str) -> Option<&'a Value> {
        owner.and_then(|owner| owner.get(key))
    }
    let value: Value = serde_json::from_str(text).ok()?;
    let specific = value.get("hookSpecificOutput");
    // The first reason that is readable text; a malformed one is not quoted.
    let reason = |candidates: &[Option<&Value>]| {
        nonempty(
            candidates
                .iter()
                .find_map(|candidate| candidate.and_then(Value::as_str)),
        )
        .to_owned()
    };
    if value.get("continue") == Some(&Value::Bool(false)) {
        let why = reason(&[value.get("stopReason")]);
        return Some(HookError::Refused(format!(
            "hook stopped processing: {why}"
        )));
    }
    let request = field(specific, "decision").filter(|_| nested);
    let deny = matches!(
        value.get("decision").and_then(Value::as_str),
        Some("deny" | "block")
    ) || field(specific, "permissionDecision").and_then(Value::as_str) == Some("deny")
        || field(request, "behavior").and_then(Value::as_str) == Some("deny");
    deny.then(|| {
        let why = reason(&[
            field(specific, "permissionDecisionReason"),
            field(request, "message"),
            value.get("reason"),
        ]);
        HookError::Refused(format!("hook denied: {why}"))
    })
}

fn nonempty(text: Option<&str>) -> &str {
    text.map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("no reason supplied")
}
