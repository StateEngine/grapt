use super::wake_custody::*;
use grapt_types::{IdAuthority, SessionId};
use std::path::PathBuf;

type TestResult = Result<(), Box<dyn std::error::Error>>;
struct Root(PathBuf);
impl Root {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let ids = grapt_store::OsRandomIds::seed()?;
        let root = std::env::temp_dir().join(format!("undo-custody-{}", ids.mint().as_str()));
        std::fs::create_dir(&root)?;
        Ok(Self(root))
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn pu_idle_root_excludes_admitted_wakes_in_both_parent_and_nested_workspaces() -> TestResult {
    let root = Root::new()?;
    let nested = root.0.join("nested");
    std::fs::create_dir(&nested)?;
    let sibling = Root::new()?;
    let slots = WakeSlots::default();
    let session: SessionId = "ses_undo_root".parse()?;
    let other: SessionId = "ses_undo_other".parse()?;
    let wake = slots.reserve_for_workspace(&session, &nested)?;
    assert!(slots.reserve_workspace_idle(&root.0).is_err());
    assert!(slots.reserve_workspace_idle(&nested).is_err());
    let _independent = slots.reserve_workspace_idle(&sibling.0)?;
    drop(wake);
    let idle = slots.reserve_workspace_idle(&root.0)?;
    assert!(slots.reserve_for_workspace(&session, &root.0).is_err());
    assert!(slots.reserve_for_workspace(&other, &nested).is_err());
    assert!(slots.reserve_workspace_idle(&nested).is_err());
    drop(idle);
    let wake = slots.reserve_for_workspace(&other, &root.0)?;
    assert!(slots.reserve_workspace_idle(&nested).is_err());
    drop(wake);
    let _idle = slots.reserve_workspace_idle(&nested)?;
    Ok(())
}

#[tokio::test]
async fn pu_a_previously_unreadable_root_must_bind_custody_before_promotion() -> TestResult {
    let root = Root::new()?;
    let missing = root.0.join("new-workspace");
    let slots = WakeSlots::default();
    let session: SessionId = "ses_undo_bind".parse()?;
    // Admission keeps the established not-run-yet behavior for unreadable roots.
    let wake = slots.reserve_for_workspace(&session, &missing)?;
    let idle = slots.reserve_workspace_idle(&root.0)?;
    std::fs::create_dir(&missing)?;
    let (started, _response) = crate::dogfood::prompt_driver::Started::channel();
    let (_shutdown, stopped) = tokio::sync::watch::channel(false);
    let mut wake = wake
        .enter(&started, stopped)
        .await?
        .ok_or("wake was cancelled")?;
    assert!(wake.bind_workspace(&slots, &missing).is_err());
    drop(idle);
    let key = wake.bind_workspace(&slots, &missing)?;
    assert_eq!(key, workspace_key(&missing)?);
    assert!(slots.reserve_workspace_idle(&root.0).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn pu_workspace_aliases_cannot_bypass_an_idle_owner() -> TestResult {
    let root = Root::new()?;
    let holder = Root::new()?;
    let alias = holder.0.join("alias");
    std::os::unix::fs::symlink(&root.0, &alias)?;
    let slots = WakeSlots::default();
    let _idle = slots.reserve_workspace_idle(&alias)?;
    let session: SessionId = "ses_undo_alias".parse()?;
    assert!(slots.reserve_for_workspace(&session, &root.0).is_err());
    Ok(())
}
