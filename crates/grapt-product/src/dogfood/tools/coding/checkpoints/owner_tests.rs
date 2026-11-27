use super::*;
use crate::dogfood::tools::coding::checkpoints::manifest::{capture_files, save, Manifest};
use grapt_store::{OsRandomIds, SystemClock};

type TestResult = Result<(), Box<dyn std::error::Error>>;
struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn pu_native_restore_authority_matches_the_exact_operation_and_capability_once() -> TestResult {
    let ids = Arc::new(OsRandomIds::seed()?);
    let clock = Arc::new(SystemClock);
    let root = Root(std::env::temp_dir().join(format!("undo-authority-{}", ids.mint().as_str())));
    let service = root.0.join("profile");
    let project = root.0.join("project");
    std::fs::create_dir_all(&service)?;
    std::fs::create_dir(&project)?;
    let project = std::fs::canonicalize(project)?;
    let artifacts = Arc::new(FilesystemArtifactStore::open(
        service.join("artifacts"),
        vec![7; 32],
        Box::new(SystemClock),
    )?);
    let input: MessageId = "msg_undo_authority".parse()?;
    let path = PathBuf::from("a.txt");
    std::fs::write(project.join(&path), b"before")?;
    let before = capture_files(
        &artifacts,
        project.clone(),
        input.clone(),
        std::slice::from_ref(&path),
    )?;
    std::fs::write(project.join(&path), b"after")?;
    let after = capture_files(
        &artifacts,
        project.clone(),
        input.clone(),
        std::slice::from_ref(&path),
    )?;
    let target = save(
        &artifacts,
        &Manifest {
            root: project.clone(),
            input: input.clone(),
            origin: Some(super::super::origin::Origin::observation(
                &grapt_checkpoint::VcsObserver::observe(
                    &mut Observer {
                        root: project.clone(),
                    },
                    &PathIdentity::canonical(project.to_string_lossy()),
                ),
            )),
            files: before.files,
            expected: Some(after.files),
        },
    )?;
    let selected = UndoSelection {
        input,
        target,
        paths: vec![path],
        incomplete_patches: 0,
    };
    let workspace = Workspace::open(&service, &project, artifacts, clock, ids)?;
    let plan = workspace
        .product
        .plan_restore(workspace.request(&selected)?)?;
    let another_plan = workspace
        .product
        .plan_restore(workspace.request(&selected)?)?;
    assert_ne!(
        plan.required_capability(),
        another_plan.required_capability()
    );
    let mut authority = workspace.authority.clone();
    assert!(matches!(
        authority.decide(&plan),
        RestoreAuthorityDecision::Refused
    ));
    let permission: PermissionId = "per_undo_authority".parse()?;
    for (operation, capability) in [
        (
            CheckpointOperationId::new("wrong_operation").ok_or("operation identity")?,
            plan.required_capability().clone(),
        ),
        (
            plan.operation().clone(),
            another_plan.required_capability().clone(),
        ),
    ] {
        *authority.0.lock().map_err(|_| "authority owner poisoned")? = Some(Approved {
            operation,
            capability,
            permission: permission.clone(),
        });
        assert!(matches!(
            authority.decide(&plan),
            RestoreAuthorityDecision::Refused
        ));
        assert!(authority
            .0
            .lock()
            .map_err(|_| "authority owner poisoned")?
            .is_none());
    }
    *authority.0.lock().map_err(|_| "authority owner poisoned")? = Some(Approved {
        operation: plan.operation().clone(),
        capability: plan.required_capability().clone(),
        permission,
    });
    assert!(matches!(
        authority.decide(&plan),
        RestoreAuthorityDecision::Granted { .. }
    ));
    assert!(matches!(
        authority.decide(&plan),
        RestoreAuthorityDecision::Refused
    ));
    Ok(())
}
