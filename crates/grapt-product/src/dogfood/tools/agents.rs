// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: parent-scoped durable operation view, bounded pagination.

use super::*;
use grapt_types::SubagentOperationId;

pub(super) const LIST_AGENTS_TOOL: &str = "list_agents";
pub(super) const LIST_AGENTS_DESCRIPTION: &str = "List self/parent IDs and direct children, including settled states; no transcripts. Pages ≤ 64; pass next_cursor as cursor.";

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct ListAgentsArguments {
    pub(super) cursor: Option<NonEmptyString>,
}

impl ToolArguments for ListAgentsArguments {
    const TOOL_NAME: &'static str = LIST_AGENTS_TOOL;

    fn provider_schema() -> String {
        // Responses strict mode requires every published property, so the
        // cursor must accept null; null means the first page, never an
        // invented token.
        serde_json::json!({"type":"object","additionalProperties":false,
            "properties":{
                "cursor":{"type":["string","null"],"minLength":1,"description":"next_cursor from the previous page; null for the first page."}
            },
            "required":[]}).to_string()
    }
}

pub(super) fn prepare_list(
    root: &Path,
    input: &BoundedToolInput,
) -> Result<PreparedEffect, String> {
    let args: ListAgentsArguments = decode_input(input)?;
    let after = args
        .cursor
        .map(|cursor| {
            if cursor.as_str().len() > 512 {
                return Err("agent cursor exceeds 512 bytes".to_owned());
            }
            SubagentOperationId::new(cursor.into_inner())
                .ok_or_else(|| "invalid agent cursor".to_owned())
        })
        .transpose()?;
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        target: EffectTarget::AgentList { after },
        capability: CapabilityId::derive(&resolved),
        action: LIST_AGENTS_TOOL.to_owned(),
        resource: "direct child agent states".to_owned(),
    })
}

pub(super) fn run_list(
    seam: &crate::dogfood::subagent::ChildSeam,
    parent: &SessionId,
    after: Option<&SubagentOperationId>,
) -> Result<Vec<u8>, String> {
    let application = seam
        .get()
        .ok_or("the child execution seam was never bound")?
        .upgrade()
        .ok_or("the application is gone; child states are unavailable")?;
    let held = application.lock_inbox();
    let page = grapt_subagent::SubagentService::list_children_on(held.store(), parent, after)
        .map_err(|error| error.to_string())?;
    let agents: Vec<_> = page
        .children
        .iter()
        .map(|child| {
            let queued =
                grapt_subagent::SubagentService::queued_followups_on(held.store(), &child.child)
                    .map_err(|error| error.to_string())?;
            let latest =
                grapt_subagent::SubagentService::latest_followup_on(held.store(), &child.child)
                    .map_err(|error| error.to_string())?;
            Ok(serde_json::json!({
                "agent_id": child.child.as_str(),
                "operation_id": child.operation.as_str(),
                "status": child.status,
                "mode": child.mode.as_str(),
                "queued_followups": queued,
                "latest_followup": latest.as_ref().map(crate::dogfood::followup::receipt_json),
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let caller = grapt_subagent::SubagentService::child_context_on(held.store(), parent)
        .map_err(|error| error.to_string())?;
    serde_json::to_vec(&serde_json::json!({
        "agent_id": parent.as_str(),
        "parent_agent_id": caller.as_ref().map(|context| context.parent.as_str()),
        "agents": agents,
        "next_cursor": page.next.as_ref().map(SubagentOperationId::as_str),
    }))
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_and_unknown_arguments_are_rejected() -> Result<(), String> {
        for raw in [r#"{"cursor":""}"#, r#"{"parent":"ses_foreign"}"#] {
            let input = BoundedToolInput::new(raw.as_bytes().to_vec())
                .map_err(|error| error.to_string())?;
            assert!(prepare_list(Path::new("/tmp"), &input).is_err());
        }
        let raw = serde_json::json!({"cursor": "x".repeat(513)}).to_string();
        let input = BoundedToolInput::new(raw.into_bytes()).map_err(|error| error.to_string())?;
        assert!(prepare_list(Path::new("/tmp"), &input).is_err());
        Ok(())
    }

    #[test]
    fn strict_mode_null_fill_lists_from_the_first_page() -> Result<(), String> {
        let mut schema: serde_json::Value =
            serde_json::from_str(&ListAgentsArguments::provider_schema())
                .map_err(|error| error.to_string())?;
        for keyword in ["anyOf", "oneOf", "allOf"] {
            assert!(schema.get(keyword).is_none());
        }
        schema["required"] = serde_json::json!(["cursor"]);
        let value = serde_json::json!({"cursor":null});
        schema::validate(
            &schema.to_string(),
            &serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )?;
        let input = BoundedToolInput::new(value.to_string().into_bytes())
            .map_err(|error| error.to_string())?;
        prepare_list(Path::new("/tmp"), &input)?;
        Ok(())
    }
}
