//! A create that fails after opening its file makes the turn's undo unavailable.
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

fn prepared(
    root: &Path,
    input: &BoundedToolInput,
    text: bool,
) -> Result<patch::PatchPlan, Box<dyn std::error::Error>> {
    let prepared = if text {
        patch::apply_patch::prepare(root, input)?
    } else {
        patch::prepare(root, input)?
    };
    let crate::dogfood::tools::EffectTarget::Coding(super::super::CodingTarget::Patch(plan)) =
        prepared.target
    else {
        return Err("fixture did not prepare a patch".into());
    };
    Ok(plan)
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn pu_incomplete_creates_make_prompt_undo_unavailable() -> Result<(), Box<dyn std::error::Error>> {
    for format in 0..3 {
        let text = format == 1;
        let ordinary = format == 2;
        let ids = Arc::new(OsRandomIds::seed()?);
        let clock = Arc::new(SystemClock);
        let scratch =
            Scratch(std::env::temp_dir().join(format!("undo-uncertain-{}", ids.mint().as_str())));
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
        let mut store = EventStore::open_in_memory()?;
        let seed = grapt_session::SessionSeed::new(
            grapt_types::ProjectId::new("prj_uncertain_fixture").ok_or("project")?,
            "/fixture",
            "uncertain create",
        )?;
        let authorities = grapt_types::DeterministicAuthorities::new(&*clock, &*ids);
        let session: SessionId =
            grapt_session::create_session(&mut store, &authorities, &seed, &mut [])?.session;
        let inbox = Mutex::new(Inbox::new(store)?);
        let input: MessageId = "msg_uncertain".parse()?;
        let call = |name: &str| ToolCallId::new(name).ok_or("call identity");
        let context = |call| FileContext {
            service: &service,
            artifacts: &artifacts,
            inbox: &inbox,
            clock: clock.clone(),
            ids: ids.clone(),
            session: &session,
            input: &input,
            call,
        };
        // An earlier complete patch in the same turn would otherwise be the
        // whole undo proposal, silently leaving the uncertain file behind.
        let first = BoundedToolInput::new(serde_json::to_vec(&serde_json::json!({"operations":[
            {"op":"create","path":"a.txt","content":"complete agent create"}
        ]}))?)?;
        let first_call = call("call_complete")?;
        run(
            context(&first_call),
            &root,
            &prepared(&root, &first, false)?,
            &first,
        )
        .map_err(|failure| failure.detail)?;
        let failing = if ordinary {
            serde_json::json!({"path":"created.txt","content":"bytes"})
        } else if text {
            serde_json::json!({"input":"*** Begin Patch\n*** Add File: created.txt\n+bytes\n*** End Patch"})
        } else {
            serde_json::json!({"operations":[{"op":"create","path":"created.txt","content":"bytes"}]})
        };
        let failing = BoundedToolInput::new(serde_json::to_vec(&failing)?)?;
        let plan = if ordinary {
            None
        } else {
            Some(prepared(&root, &failing, text)?)
        };
        let failing_call = call("call_uncertain")?;
        super::super::patch::CREATE_WRITE_FAULT.with(|fault| fault.set(true));
        let result = if let Some(plan) = plan {
            run(context(&failing_call), &root, &plan, &failing)
        } else {
            let crate::dogfood::tools::EffectTarget::Coding(target) =
                super::super::prepare(&root, crate::dogfood::tools::WRITE_TOOL, &failing)?.target
            else {
                return Err("fixture did not prepare an ordinary write".into());
            };
            run_file(
                context(&failing_call),
                &root,
                super::super::file_effects::prepare_write(&root, &target, &failing)?,
            )
        };
        let failure = result.err().ok_or("the injected create failure applied")?;
        assert!(
            failure.detail.contains("injected create write failure"),
            "{}",
            failure.detail
        );
        assert!(
            root.join("created.txt").try_exists()?,
            "the fault must fire after the create opened its file"
        );
        let workspace = Workspace::open(
            &service,
            &root,
            Arc::clone(&artifacts),
            clock.clone(),
            ids.clone(),
        )?;
        let selection = workspace.select(inbox.lock().map_err(|_| "inbox")?.store(), &session);
        let refusal = selection
            .err()
            .ok_or("undo selected a turn whose create left uncertain bytes")?;
        assert!(
            refusal.contains("file undo is unavailable") && refusal.contains("uncertain"),
            "text={text}, ordinary={ordinary}: {refusal}"
        );
    }
    Ok(())
}
