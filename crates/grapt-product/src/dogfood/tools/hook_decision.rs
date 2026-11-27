//! Hook decisions enter the ordinary preparation and permission path once.
use super::*;
use crate::dogfood::hooks::{Event, PermissionDecision};
use grapt_permission::preset::{Compose, Effect, Layer, LayerName, Rule};

impl PermissionToolRunner {
    pub(super) fn run_prepared(
        &self,
        attempt: Option<grapt_types::PhysicalAttemptId>,
        call: ToolCallId,
        tool: String,
        input: BoundedToolInput,
        mut options: ToolRunOptions,
        nested: Option<grapt_runtime::NestedTools>,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        let runner = self.clone();
        Box::pin(async move {
            if !runner.policy.permits_agent_tool(&tool) {
                return runner
                    .early_failure(
                        &tool,
                        ToolFailureReason::AuthorityUnavailable,
                        "tool is excluded by this agent or an ancestor's tool list".into(),
                    )
                    .await;
            }
            let input = if let Some(runtime) = &runner.hooks {
                let value = match serde_json::from_slice(input.as_bytes()) {
                    Ok(value) => value,
                    Err(_) => {
                        return runner
                            .early_failure(
                                &tool,
                                ToolFailureReason::EffectFailed,
                                "tool input is invalid JSON; no hook or effect ran".into(),
                            )
                            .await
                    }
                };
                let alias = runner
                    .published
                    .routes
                    .get(&tool)
                    .map(|route| format!("mcp__{}__{}", route.server, route.tool.as_str()));
                options.hook = match runtime
                    .fire_tool(
                        Event::PreToolUse,
                        &runner.session,
                        (&tool, &call),
                        &value,
                        alias.as_deref(),
                    )
                    .await
                {
                    Ok(output) => output,
                    Err(reason) => {
                        return runner
                            .early_failure(
                                &tool,
                                ToolFailureReason::EffectFailed,
                                format!("PreToolUse hook refused; no effect ran: {reason}"),
                            )
                            .await
                    }
                };
                if let Some(value) = &options.hook.input {
                    let encoded = serde_json::to_vec(value)
                        .map_err(|_| "hook updatedInput encoding failed".to_owned())
                        .and_then(|bytes| {
                            BoundedToolInput::new(bytes).map_err(|error| error.to_string())
                        });
                    match encoded {
                        Ok(input) => input,
                        Err(reason) => return runner.rejected_hook_input(&tool, &reason).await,
                    }
                } else {
                    input
                }
            } else {
                input
            };
            // Revalidate the replacement against the exact public schema and
            // strict built-in decoder before resolving its permission targets.
            runner
                .prepare_and_authorize(attempt, call, tool, input, options, nested)
                .await
        })
    }

    pub(super) fn rejected_hook_input(
        &self,
        tool: &str,
        reason: &str,
    ) -> BoxFuture<'static, ToolTerminalOutcome> {
        let recorded = self.hooks.as_ref().map_or(Ok(()), |runtime| {
            runtime.rejected_input(&self.session, reason)
        });
        let detail = match recorded {
            Ok(()) => format!("hook modified input rejected; no effect ran: {reason}"),
            Err(error) => {
                format!("hook modified input rejected; receipt failed: {error}; no effect ran")
            }
        };
        self.early_failure(tool, ToolFailureReason::EffectFailed, detail)
    }
}

pub(super) fn layers(
    layers: &mut Vec<Layer>,
    checks: &[grapt_permission::PermissionConsequence],
    decision: PermissionDecision,
) {
    let effect = match decision {
        PermissionDecision::Default => return,
        PermissionDecision::Allow => Effect::Allow,
        PermissionDecision::Ask => {
            // Even an explicit bypass preset cannot suppress a user hook's ask.
            layers.retain(|layer| layer.name != LayerName::Bypass);
            Effect::Ask
        }
    };
    layers.push(Layer {
        name: LayerName::Config,
        rules: checks
            .iter()
            .map(|check| Rule::literal(&check.action, &check.resource, effect))
            .collect(),
        floor: decision == PermissionDecision::Ask,
        compose: if decision == PermissionDecision::Allow {
            Compose::Override
        } else {
            Compose::Narrow
        },
    });
}
