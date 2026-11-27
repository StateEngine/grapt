//! Receipt envelope limits are checked before the patch's first file effect.
use super::*;
use grapt_runtime::BoundedToolInput;
use grapt_store::{EventStore, OsRandomIds, SystemClock};
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn pu_oversized_receipt_coordinates_refuse_before_changing_files(
) -> Result<(), Box<dyn std::error::Error>> {
    let ids = Arc::new(OsRandomIds::seed()?);
    let clock = Arc::new(SystemClock);
    let scratch =
        Scratch(std::env::temp_dir().join(format!("undo-boundary-{}", ids.mint().as_str())));
    let service = scratch.0.join("profile");
    let root = scratch.0.join("workspace");
    std::fs::create_dir_all(&service)?;
    std::fs::create_dir(&root)?;
    let root = std::fs::canonicalize(root)?;
    let artifacts = Arc::new(FilesystemArtifactStore::open(
        service.join("artifacts"),
        vec![7; 32],
        Box::new(SystemClock),
    )?);
    let inbox = Mutex::new(Inbox::new(EventStore::open_in_memory()?)?);
    let session: SessionId = "ses_boundary".parse()?;
    let input: MessageId = "msg_boundary".parse()?;
    let call = ToolCallId::new("call_boundary").ok_or("call identity")?;
    let large_session: SessionId = format!("ses_{}", "x".repeat(509)).parse()?;
    let large_input: MessageId = format!("msg_{}", "x".repeat(509)).parse()?;
    let large_call =
        ToolCallId::new(format!("call_{}", "x".repeat(508))).ok_or("long call identity")?;
    let arguments = BoundedToolInput::new(serde_json::to_vec(&serde_json::json!({"operations":[
        {"op":"create","path":"created.txt","content":"must not be written"}
    ]}))?)?;
    let crate::dogfood::tools::EffectTarget::Coding(super::super::CodingTarget::Patch(plan)) =
        patch::prepare(&root, &arguments)?.target
    else {
        return Err("fixture did not prepare a patch".into());
    };
    for (session, input, call) in [
        (&large_session, &input, &call),
        (&session, &large_input, &call),
        (&session, &input, &large_call),
    ] {
        let result = run(
            FileContext {
                service: &service,
                artifacts: &artifacts,
                inbox: &inbox,
                clock: clock.clone(),
                ids: ids.clone(),
                session,
                input,
                call,
            },
            &root,
            &plan,
            &arguments,
        );
        assert!(
            !root.join("created.txt").try_exists()?,
            "an invalid receipt identity changed files before refusal"
        );
        let failure = result
            .err()
            .ok_or("oversized identity unexpectedly applied")?;
        assert_eq!(failure.reason, ToolFailureReason::EffectFailed);
        assert!(
            failure.detail.contains("before changing files"),
            "{}",
            failure.detail
        );
    }
    Ok(())
}
