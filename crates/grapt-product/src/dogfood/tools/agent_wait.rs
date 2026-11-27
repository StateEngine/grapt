// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: durable result checks and bounded advisory activity.

use super::*;

pub(super) const WAIT_AGENT_TOOL: &str = "wait_agent";
pub(super) const WAIT_AGENT_DESCRIPTION: &str = "Wait for mail, input or timeout_ms (default 30000, range 1000–60000). Results/messages arrive automatically.";

tool_arguments! {
    pub(super) struct WaitAgentArguments => WAIT_AGENT_TOOL {
        optional timeout_ms: BoundedU64<1000, 60000>,
    }
}

pub(super) fn prepare(root: &Path, input: &BoundedToolInput) -> Result<PreparedEffect, String> {
    let args: WaitAgentArguments = decode_input(input)?;
    let resolved = ResolvedCapability::new(
        EffectClass::ToolInvoke,
        None,
        FilesystemAuthority::new(Vec::new(), Vec::new(), protected_paths(root)),
        EgressAuthority::default(),
        RealizedEnforcement::HarnessOnly,
    );
    Ok(PreparedEffect {
        capability: CapabilityId::derive(&resolved),
        target: EffectTarget::AgentWait {
            timeout_ms: args.timeout_ms.map_or(30000, BoundedU64::get),
        },
        action: WAIT_AGENT_TOOL.into(),
        resource: "agent mailbox activity".into(),
    })
}

pub(in crate::dogfood) async fn run(
    seam: &crate::dogfood::subagent::ChildSeam,
    parent: &SessionId,
    timeout_ms: u64,
    cancellation: &ToolCancellation,
) -> Result<Vec<u8>, String> {
    let application = seam
        .get()
        .ok_or("the child execution seam was never bound")?
        .upgrade()
        .ok_or("the application is gone; agent mail is unavailable")?;
    // Subscribe before the durable read: a later change either appears in that
    // read or remains an unseen advisory. Advisories never own mailbox truth.
    let mut activity = application.agent_activity.subscribe();
    let mut cancellation = cancellation.clone();
    let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(deadline);
    let (message, timed_out) = loop {
        if cancellation.cancelled_by().is_some() {
            return Err("agent wait cancelled".into());
        }
        if crate::dogfood::agent_mail::reconcile(&application.inbox, parent)? {
            break ("Wait completed.", false);
        }
        if !application
            .lock_inbox()
            .pending_inputs(parent)
            .map_err(|error| error.to_string())?
            .is_empty()
        {
            break ("Wait interrupted by new input.", false);
        }
        tokio::select! {
            biased;
            _ = &mut deadline => break ("Wait timed out.", true),
            _ = cancellation.changed() => return Err("agent wait cancelled".into()),
            changed = activity.changed() => {
                changed.map_err(|_| "agent activity is unavailable")?;
            }
        }
    };
    serde_json::to_vec(&serde_json::json!({"message":message,"timed_out":timed_out}))
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timeout_and_argument_bounds_are_explicit() -> Result<(), String> {
        for raw in [
            r#"{"timeout_ms":0}"#,
            r#"{"timeout_ms":60001}"#,
            r#"{"timeout_ms":null}"#,
            r#"{"timeout_ms":-1}"#,
            r#"{"target":"ses_foreign"}"#,
        ] {
            let input = BoundedToolInput::new(raw.as_bytes().to_vec())
                .map_err(|error| error.to_string())?;
            assert!(prepare(Path::new("/tmp"), &input).is_err());
        }
        for raw in ["{}", r#"{"timeout_ms":1000}"#, r#"{"timeout_ms":60000}"#] {
            let input = BoundedToolInput::new(raw.as_bytes().to_vec())
                .map_err(|error| error.to_string())?;
            assert!(prepare(Path::new("/tmp"), &input).is_ok());
        }
        Ok(())
    }
}
