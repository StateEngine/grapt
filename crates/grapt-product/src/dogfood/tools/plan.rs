//! Checklist arguments.
//! Copyright 2025 OpenAI. Licensed under Apache-2.0; see licenses/openai-codex.
//! Modified by Grapt: bounded validation and existing governed history integration.
use super::*;
use serde::Serialize;

pub(super) const ACKNOWLEDGEMENT: &str = "Plan updated";

pub(super) const DESCRIPTION: &str = "Replace plan: pending/in_progress/completed, at most one in_progress. Empty clears. ≤64 steps, 1024 bytes each; explanation≤4096 bytes.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StepStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanItemArg {
    step: String,
    status: StepStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdatePlanArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    explanation: Option<String>,
    plan: Vec<PlanItemArg>,
}

impl ToolArguments for UpdatePlanArgs {
    const TOOL_NAME: &'static str = PLAN_TOOL;

    fn provider_schema() -> String {
        serde_json::json!({
            "type":"object", "additionalProperties":false, "required":["plan"],
            "properties": {
                "explanation":{"type":["string","null"], "maxLength":4096},
                "plan":{"type":"array", "maxItems":64, "items":{
                    "type":"object", "additionalProperties":false,
                    "required":["step","status"], "properties":{
                        "step":{"type":"string","minLength":1,"maxLength":1024},
                        "status":{"type":"string","enum":["pending","in_progress","completed"]}
                    }
                }}
            }
        })
        .to_string()
    }
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: UpdatePlanArgs = decode_input(input)?;
    if args.plan.len() > 64
        || args
            .plan
            .iter()
            .any(|item| item.step.trim().is_empty() || item.step.len() > 1024)
        || args
            .explanation
            .as_ref()
            .is_some_and(|text| text.len() > 4096)
    {
        return Err(
            "plan requires at most 64 nonempty steps of 1024 bytes and explanation of 4096 bytes"
                .into(),
        );
    }
    if args
        .plan
        .iter()
        .filter(|item| item.status == StepStatus::InProgress)
        .count()
        > 1
    {
        return Err("at most one plan step may be in_progress".into());
    }
    // Input and successful terminal are already committed by the tool owner.
    // Return a short acknowledgement, not another copy of the plan in
    // the model's context. The TUI reconstructs the checklist from that input.
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        target: EffectTarget::Plan,
        action: PLAN_TOOL.to_owned(),
        resource: "task checklist".to_owned(),
    })
}

#[cfg(test)]
mod tests;
