// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: existing admission and bounded prompt execution owners.

use super::*;

pub(super) const SPAWN_AGENT_TOOL: &str = "spawn_agent";
pub(super) const SPAWN_AGENT_DESCRIPTION: &str = "Background child; returns agent_id. agent_type names a role; omitted inherits a child-capable parent; primary agents must name a child role. Permissions inherit. task_name: 1..64 [a-z0-9_]. worktree=true isolates HEAD (no parent edits), else shares cwd. fork_turns: all (default), positive count or none.";

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct SpawnAgentArguments {
    pub(super) task_name: NonEmptyString,
    pub(super) message: NonEmptyString,
    pub(super) fork_turns: Option<NonEmptyString>,
    pub(super) worktree: Option<bool>,
    pub(super) agent_type: Option<NonEmptyString>,
}

impl ToolArguments for SpawnAgentArguments {
    const TOOL_NAME: &'static str = SPAWN_AGENT_TOOL;

    fn provider_schema() -> String {
        // Responses strict mode requires every published property. fork_turns
        // and worktree have safe defaults the model can fill; agent_type has
        // no safe invented name, so it must accept null to inherit the parent.
        serde_json::json!({"type":"object","additionalProperties":false,
            "properties":{
                "task_name":{"type":"string","minLength":1},
                "message":{"type":"string","minLength":1},
                "fork_turns":{"type":"string","minLength":1},
                "worktree":{"type":"boolean"},
                "agent_type":{"type":["string","null"],"minLength":1,"description":"Role name from the agent definitions; null inherits a child-capable parent; primary agents must name a subagent."}
            },
            "required":["task_name","message"]}).to_string()
    }
}

#[derive(Debug, Clone)]
pub(super) struct SpawnTask {
    task_name: String,
    message: String,
    worktree: bool,
    fork_history: bool,
    agent_type: Option<AgentId>,
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: SpawnAgentArguments = decode_input(input)?;
    if args.task_name.as_str().len() > 64
        || !args
            .task_name
            .as_str()
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err("task_name must use 1–64 lowercase letters, digits or underscores".into());
    }
    if args.message.as_str().trim().is_empty() || args.message.as_str().len() > 64 * 1024 {
        return Err("agent message must contain text and fit within 64 KiB".into());
    }
    let fork = args
        .fork_turns
        .as_ref()
        .map_or("all", |fork| fork.as_str().trim());
    let fork_history = if fork.eq_ignore_ascii_case("none") {
        false
    } else if fork.eq_ignore_ascii_case("all") || fork.parse::<std::num::NonZeroUsize>().is_ok() {
        true
    } else {
        return Err("fork_turns must be none or all".into());
    };
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    let task = SpawnTask {
        task_name: args.task_name.into_inner(),
        message: args.message.into_inner(),
        worktree: args.worktree.unwrap_or(false),
        fork_history,
        agent_type: args
            .agent_type
            .map(|name| {
                if !crate::dogfood::agent_definitions::valid_name(name.as_str()) {
                    return Err("agent_type must be a named agent".to_owned());
                }
                AgentId::new(name.into_inner()).ok_or_else(|| "agent_type is invalid".to_owned())
            })
            .transpose()?,
    };
    let placement = if task.worktree {
        " (new detached worktree)"
    } else {
        ""
    };
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        resource: format!("{}{placement}: {}", task.task_name, task.message),
        target: EffectTarget::AgentSpawn(task),
        action: SPAWN_AGENT_TOOL.into(),
    })
}

pub(super) async fn run(
    seam: &crate::dogfood::subagent::ChildSeam,
    parent: &SessionId,
    agent: &AgentId,
    principal: &SealedPrincipal,
    call: &ToolCallId,
    task: &SpawnTask,
) -> Result<Vec<u8>, String> {
    use crate::dogfood::subagent::{admit_with_options, ChildOptions};
    use grapt_api::u3a::WireId;

    let application = seam
        .get()
        .ok_or("the child execution seam was never bound")?
        .upgrade()
        .ok_or("the application is gone; no child can be started")?;
    let request = crate::dogfood::support::decode(serde_json::json!({
        "text": task.message,
        "delivery": "steer",
        "resume": true,
    }))?;
    let mut options = ChildOptions::new(grapt_subagent::SubagentMode::Background);
    if task.fork_history {
        options = options.with_history_before(call.clone());
    }
    if task.worktree {
        options = options.with_worktree(&application, parent).await?;
    }
    let admitted = admit_with_options(
        &application,
        parent,
        task.agent_type.as_ref().unwrap_or(agent),
        &task.task_name,
        Some(call.clone()),
        &options,
    )?;
    let directory = task
        .worktree
        .then(|| application.get_record(&admitted.session))
        .transpose()?
        .map(|record| record.placement.directory().to_owned());
    let child = WireId::parse(admitted.session.as_str().to_owned())
        .ok_or("the admitted child has an invalid address")?;
    // Submission transfers the owned future to the bounded prompt driver before
    // awaiting custody. Dropping this tool after submission leaves that owner
    // responsible for the child and its durable terminal.
    let execution = application.mint_execution()?;
    if let Err(error) = application
        .submit_child_prompt(principal.clone(), child, request, execution)
        .await
    {
        let error = format!("the background child could not start: {error:?}");
        crate::dogfood::subagent::settle(
            &application,
            &admitted.operation,
            &grapt_subagent::SubagentTerminal::Failed {
                error: error.clone(),
            },
        )?;
        return Err(error);
    }
    let mut result = serde_json::json!({
        "agent_id": admitted.session.as_str(),
        "operation_id": admitted.operation.as_str(),
    });
    if let Some(directory) = directory {
        result["directory"] = directory.into();
    }
    serde_json::to_vec(&result).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_history_options_and_task_bounds_are_explicit() -> Result<(), String> {
        for raw in [
            serde_json::json!({"task_name":"read", "message":" "}),
            serde_json::json!({"task_name":"../read", "message":"read the diff"}),
            serde_json::json!({"task_name":"read", "message":"read", "fork_turns":"0"}),
            serde_json::json!({"task_name":"read", "message":"read", "fork_turns":"-3"}),
            serde_json::json!({"task_name":"read", "message":"read", "model":"other"}),
            serde_json::json!({"task_name":"a".repeat(65), "message":"read"}),
            serde_json::json!({"task_name":"read", "message":"read", "worktree":"true"}),
        ] {
            let input = BoundedToolInput::new(raw.to_string().into_bytes())
                .map_err(|error| error.to_string())?;
            assert!(prepare(Path::new("/tmp"), &input).is_err());
        }
        for fork in [None, Some("none"), Some("all"), Some("3"), Some("ALL")] {
            let mut raw = serde_json::json!({"task_name":"read_diff", "message":"read the diff"});
            if let Some(fork) = fork {
                raw["fork_turns"] = serde_json::json!(fork);
            }
            let input = BoundedToolInput::new(raw.to_string().into_bytes())
                .map_err(|error| error.to_string())?;
            let prepared = prepare(Path::new("/tmp"), &input)?;
            let EffectTarget::AgentSpawn(task) = prepared.target else {
                return Err("spawn target".into());
            };
            assert_eq!(task.fork_history, fork != Some("none"));
        }
        Ok(())
    }

    #[test]
    fn worktree_choice_is_explicit_and_bound_to_the_prepared_effect() -> Result<(), String> {
        for choice in [None, Some(false), Some(true)] {
            let mut raw = serde_json::json!({"task_name":"read", "message":"inspect file.txt"});
            if let Some(choice) = choice {
                raw["worktree"] = choice.into();
            }
            let input = BoundedToolInput::new(raw.to_string().into_bytes())
                .map_err(|error| error.to_string())?;
            let effect = prepare(Path::new("/tmp"), &input)?;
            let EffectTarget::AgentSpawn(task) = effect.target else {
                return Err("spawn prepared another effect".into());
            };
            assert_eq!(task.worktree, choice == Some(true));
            assert_eq!(
                effect.resource.contains("new detached worktree"),
                task.worktree
            );
        }
        Ok(())
    }

    #[test]
    fn strict_mode_null_fill_inherits_the_parent_agent() -> Result<(), String> {
        let mut schema: serde_json::Value =
            serde_json::from_str(&SpawnAgentArguments::provider_schema())
                .map_err(|error| error.to_string())?;
        for keyword in ["anyOf", "oneOf", "allOf"] {
            assert!(schema.get(keyword).is_none());
        }
        // The hosted Responses route forces every published property; the
        // only invented-value trap is agent_type, so it alone accepts null.
        schema["required"] = serde_json::json!([
            "task_name",
            "message",
            "fork_turns",
            "worktree",
            "agent_type"
        ]);
        let value = serde_json::json!({"task_name":"probe","message":"ping","fork_turns":"all","worktree":false,"agent_type":null});
        schema::validate(
            &schema.to_string(),
            &serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        )?;
        let input = BoundedToolInput::new(value.to_string().into_bytes())
            .map_err(|error| error.to_string())?;
        let prepared = prepare(Path::new("/tmp"), &input)?;
        let EffectTarget::AgentSpawn(task) = prepared.target else {
            return Err("spawn prepared another effect".into());
        };
        assert_eq!(task.agent_type, None);
        assert!(task.fork_history);
        assert!(!task.worktree);
        Ok(())
    }
}
