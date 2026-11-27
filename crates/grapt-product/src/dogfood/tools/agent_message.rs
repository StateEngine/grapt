// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bounded inbox mail without execution wake.

use super::*;

pub(super) const SEND_MESSAGE_TOOL: &str = "send_message";
pub(super) const SEND_MESSAGE_DESCRIPTION: &str = "Send text (nonblank, ≤ 64 KiB) to a known family agent_id. Quoted delivery at a safe Step; idle agents stay idle.";

tool_arguments! {
    pub(super) struct SendMessageArguments => SEND_MESSAGE_TOOL {
        required target: NonEmptyString,
        required message: NonEmptyString,
    }
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: SendMessageArguments = decode_input(input)?;
    if args.target.as_str().len() > 512
        || args.message.as_str().trim().is_empty()
        || args.message.as_str().len() > grapt_event::agent_message::MAX_TEXT_BYTES
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
        action: SEND_MESSAGE_TOOL.into(),
        resource: format!("{}: {}", target.as_str(), args.message.as_str()),
        target: EffectTarget::AgentMessage {
            target,
            text: args.message.into_inner(),
        },
    })
}

pub(super) fn run(
    seam: &crate::dogfood::subagent::ChildSeam,
    sender: &SessionId,
    target: &SessionId,
    call: &ToolCallId,
    text: &str,
) -> Result<Vec<u8>, String> {
    let application = seam
        .get()
        .ok_or("the agent execution seam was never bound")?
        .upgrade()
        .ok_or("the application is gone; agent mail is unavailable")?;
    grapt_subagent::SubagentService::authorize_message_on(
        application.lock_inbox().store(),
        sender,
        target,
    )
    .map_err(|error| error.to_string())?;
    application.get_record(target)?;
    application.ensure_session_writer(target)?;
    let receipt = grapt_subagent::SubagentService::send_message_on(
        application.lock_inbox().store_mut(),
        &grapt_subagent::AgentMessageRequest {
            sender,
            target,
            call,
            text,
            created_ms: application.clock.now().as_millis(),
        },
    )
    .map_err(|error| error.to_string())?;
    // The advisory releases a waiting tool; only the inbox fact owns the mail.
    application.agent_activity.send_replace(());
    serde_json::to_vec(&serde_json::json!({"agent_id":target.as_str(),
        "message_id":receipt.input.as_str(), "queued":true}))
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn message_arguments_are_bounded_and_cannot_claim_another_sender() -> Result<(), String> {
        for raw in [
            serde_json::json!({"target":"ses_child", "message":" "}),
            serde_json::json!({"target":"other", "message":"hello"}),
            serde_json::json!({"target":"ses_child", "message":"hello", "sender":"ses_forged"}),
            serde_json::json!({"target":"ses_child", "message":"x".repeat(65537)}),
        ] {
            let input = BoundedToolInput::new(raw.to_string().into_bytes())
                .map_err(|error| error.to_string())?;
            assert!(prepare(Path::new("/tmp"), &input).is_err());
        }
        let input = BoundedToolInput::new(br#"{"target":"ses_child","message":"hello"}"#.to_vec())
            .map_err(|error| error.to_string())?;
        assert!(prepare(Path::new("/tmp"), &input).is_ok());
        Ok(())
    }
}
