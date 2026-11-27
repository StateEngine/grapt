// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: existing bounded prompt custody and durable run claims.
use super::*;

pub(super) const FOLLOWUP_TASK_TOOL: &str = "followup_task";
pub(super) const FOLLOWUP_TASK_DESCRIPTION:&str="Assign work (nonblank, ≤ 64 KiB) to a family child. Idle children wake; active ones get quoted mail at a safe Step. Root refused; results return to the original parent.";

tool_arguments! {
    pub(super) struct FollowupTaskArguments => FOLLOWUP_TASK_TOOL {
        required target: NonEmptyString,
        required message: NonEmptyString,
    }
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: FollowupTaskArguments = decode_input(input)?;
    if args.target.as_str().len() > 512
        || args.message.as_str().len() > 64 * 1024
        || args.message.as_str().trim().is_empty()
    {
        return Err(
            "agent target must fit within 512 bytes and message must contain text within 64 KiB"
                .into(),
        );
    }
    let target: SessionId = args
        .target
        .into_inner()
        .parse()
        .map_err(|error| format!("invalid agent address: {error}"))?;
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        action: FOLLOWUP_TASK_TOOL.into(),
        resource: format!("{}: {}", target.as_str(), args.message.as_str()),
        target: EffectTarget::AgentFollowup {
            target,
            text: args.message.into_inner(),
        },
    })
}

pub(super) fn run(
    seam: &crate::dogfood::subagent::ChildSeam,
    sender: &SessionId,
    target: &SessionId,
    principal: &SealedPrincipal,
    call: &ToolCallId,
    text: &str,
) -> Result<Vec<u8>, String> {
    let application = seam
        .get()
        .ok_or("the agent execution seam was never bound")?
        .upgrade()
        .ok_or("the application is gone; agent followup is unavailable")?;
    crate::dogfood::followup::submit(&application, sender, target, principal, call, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn followup_arguments_cannot_claim_a_sender_or_supply_empty_work() -> Result<(), String> {
        for value in [
            serde_json::json!({"target":"ses_child","message":" "}),
            serde_json::json!({"target":"ses_child","message":"work","sender":"ses_forged"}),
            serde_json::json!({"target":"ses_child","message":"x".repeat(65537)}),
        ] {
            let input = BoundedToolInput::new(value.to_string().into_bytes())
                .map_err(|error| error.to_string())?;
            assert!(prepare(Path::new("/tmp"), &input).is_err());
        }
        Ok(())
    }
}
