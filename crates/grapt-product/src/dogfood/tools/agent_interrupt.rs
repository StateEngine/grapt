// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: direct-child routing through the durable operation owner.

use super::*;
use grapt_subagent::OperationStatus;

pub(super) const INTERRUPT_AGENT_TOOL: &str = "interrupt_agent";
pub(super) const INTERRUPT_AGENT_DESCRIPTION: &str = "Interrupt a direct child by agent_id; returns prior durable status. Running interruption is asynchronous; settled children stay settled.";

tool_arguments! {
    pub(super) struct InterruptAgentArguments => INTERRUPT_AGENT_TOOL {
        required target: NonEmptyString,
    }
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: InterruptAgentArguments = decode_input(input)?;
    if args.target.as_str().len() > 512 {
        return Err("agent target exceeds 512 bytes".into());
    }
    let child: SessionId = args
        .target
        .into_inner()
        .parse()
        .map_err(|error| format!("invalid child agent id: {error}"))?;
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        target: EffectTarget::AgentInterrupt {
            child: child.clone(),
        },
        action: INTERRUPT_AGENT_TOOL.into(),
        resource: child.as_str().to_owned(),
    })
}

pub(super) fn run(
    seam: &crate::dogfood::subagent::ChildSeam,
    parent: &SessionId,
    child: &SessionId,
) -> Result<Vec<u8>, String> {
    let application = seam
        .get()
        .ok_or("the child execution seam was never bound")?
        .upgrade()
        .ok_or("the application is gone; the child cannot be interrupted")?;
    let context = crate::dogfood::subagent::interrupt_target_context(&application, parent, child)?;
    application.agent_activity.send_replace(());
    let previous_status = match &context.status {
        OperationStatus::Admitted => "admitted",
        OperationStatus::Running(_) => {
            let execution = application
                .executor
                .active_executions()
                .into_iter()
                .find(|(session, _)| session == child)
                .map(|(_, execution)| execution)
                .ok_or("the child's execution is changing; retry after its next status update")?;
            application.executor.interrupt(child, Some(&execution))?;
            "running"
        }
        OperationStatus::Terminal(terminal) => terminal.status_str(),
        OperationStatus::ExternallyAmbiguous(_) => "externally-ambiguous",
    };
    serde_json::to_vec(&serde_json::json!({
        "agent_id": child.as_str(),
        "previous_status": previous_status,
    }))
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupt_requires_an_exact_bounded_child_address() -> Result<(), String> {
        for raw in [
            "{}".to_owned(),
            r#"{"target":""}"#.to_owned(),
            r#"{"target":"not-a-session"}"#.to_owned(),
            r#"{"target":"ses_child","parent":"ses_foreign"}"#.to_owned(),
            serde_json::json!({"target": format!("ses_{}", "a".repeat(512))}).to_string(),
        ] {
            let input =
                BoundedToolInput::new(raw.into_bytes()).map_err(|error| error.to_string())?;
            assert!(prepare(Path::new("/tmp"), &input).is_err());
        }
        let input = BoundedToolInput::new(br#"{"target":"ses_child"}"#.to_vec())
            .map_err(|error| error.to_string())?;
        assert!(prepare(Path::new("/tmp"), &input).is_ok());
        Ok(())
    }
}
