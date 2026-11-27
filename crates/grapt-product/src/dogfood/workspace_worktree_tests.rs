use super::*;
use grapt_types::{DeterministicAuthorities, IdAuthority, ProjectId};
use std::sync::Arc;

type TestResult = Result<(), Box<dyn std::error::Error>>;
struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn git(directory: &Path, args: &[&str]) -> TestResult {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(directory)
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .args(["-c", "user.name=Test"])
        .args(["-c", "user.email=test@example.invalid"])
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn worktree_lifecycle_uses_saved_source_and_excludes_admitted_work_in_both_checkouts(
) -> TestResult {
    let ids = grapt_store::OsRandomIds::seed()?;
    let root = Root(std::env::temp_dir().join(format!("worktree-api-{}", ids.mint().as_str())));
    let repo = root.0.join("repo");
    let source = repo.join("nested");
    std::fs::create_dir_all(&source)?;
    git(&repo, &["init", "-q"])?;
    std::fs::write(source.join("file.txt"), "initial")?;
    git(&repo, &["add", "."])?;
    git(&repo, &["commit", "-qm", "initial"])?;
    let service = root.0.join("service");
    std::fs::create_dir(&service)?;
    let (application, _driver) = crate::dogfood::test_provider::open(
        &service,
        grapt_store::EventStore::open_in_memory()?,
        &crate::dogfood::production::tests::projection_target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let authorities = DeterministicAuthorities::new(&*application.clock, &*application.ids);
    let session = grapt_runtime::SessionPorts::new(Arc::clone(&application.inbox), &authorities)
        .create(&grapt_session::SessionSeed::new(
            ProjectId::new("worktree-project").ok_or("project")?,
            source.to_str().ok_or("source encoding")?,
            "worktree",
        )?)?
        .session;
    let query: ProjectCurrentLocationQueryDto =
        decode(json!({"session":session.as_str(),"location":{"directory":repo}}))?;
    let create = || {
        decode(
            json!({"action":"create","destination":"../../checkout","setup_files":["nested/setup.local"]}),
        )
    };
    assert!(
        response(application.as_ref().clone(), decode(json!({}))?, create()?)
            .await
            .is_err()
    );
    for request in [
        json!({"action":"create","destination":""}),
        json!({"action":"create","destination":"../../repo"}),
        json!({"action":"remove","directory":"../../missing"}),
        json!({"action":"create","destination":"../../unused","setup_files":vec!["file";33]}),
    ] {
        let outcome = response(
            application.as_ref().clone(),
            query.clone(),
            decode(request)?,
        )
        .await
        .map_err(|error| error.message().get().as_str().to_owned())?;
        assert_eq!(
            *outcome.data().get().effect().get(),
            WorkspaceWorktreeEffectDto::NotStarted
        );
        assert!(!outcome.data().get().message().get().as_str().is_empty());
    }
    // The source index no longer tracks this file, but HEAD still places it in
    // the new checkout. Its no-overwrite setup copy fails after checkout creation.
    git(&repo, &["rm", "--cached", "--", "nested/file.txt"])?;
    let partial = response(application.as_ref().clone(), query.clone(),
        decode(json!({"action":"create","destination":"../../partial","setup_files":["nested/file.txt"]}))?)
        .await.map_err(|error| error.message().get().as_str().to_owned())?;
    assert_eq!(
        *partial.data().get().effect().get(),
        WorkspaceWorktreeEffectDto::MayHaveChanged
    );
    assert_eq!(
        std::fs::canonicalize(partial.data().get().directory().get().as_str())?,
        std::fs::canonicalize(&source)?
    );
    assert!(root.0.join("partial/.git").is_file());
    git(&repo, &["add", "--", "nested/file.txt"])?;
    std::fs::write(source.join("setup.local"), "explicit setup")?;
    let created = response(application.as_ref().clone(), query.clone(), create()?)
        .await
        .map_err(|error| format!("create: {}", error.message().get().as_str()))?;
    let created = serde_json::to_value(created)?;
    assert_eq!(created["data"]["effect"], "completed");
    let checkout_cwd = PathBuf::from(
        created["data"]["directory"]
            .as_str()
            .ok_or("created directory")?,
    );
    assert_eq!(
        std::fs::canonicalize(
            created["location"]["directory"]
                .as_str()
                .ok_or("source directory")?
        )?,
        std::fs::canonicalize(&source)?
    );
    assert_eq!(
        std::fs::read(source.join("setup.local"))?,
        std::fs::read(checkout_cwd.join("setup.local"))?
    );
    std::fs::remove_file(source.join("setup.local"))?;
    std::fs::remove_file(checkout_cwd.join("setup.local"))?;
    std::fs::write(checkout_cwd.join("file.txt"), "committed child")?;
    git(&checkout_cwd, &["add", "file.txt"])?;
    git(&checkout_cwd, &["commit", "-qm", "child"])?;
    for busy_directory in [&source, &checkout_cwd] {
        let _busy = application
            .wake_slots
            .reserve_for_workspace(&session, busy_directory)?;
        for action in ["merge", "remove"] {
            let result = response(
                application.as_ref().clone(),
                query.clone(),
                decode(json!({"action":action,"directory":"../../checkout"}))?,
            )
            .await;
            let outcome = result.map_err(|error| error.message().get().as_str().to_owned())?;
            let data = outcome.data().get();
            assert_eq!(*data.effect().get(), WorkspaceWorktreeEffectDto::NotStarted);
            let message = data.message().get().as_str();
            assert!(
                message.contains("requires idle"),
                "{action} with admitted work at {} returned an unexpected refusal: {message}",
                busy_directory.display()
            );
        }
    }
    for action in ["merge", "remove"] {
        let outcome = response(
            application.as_ref().clone(),
            query.clone(),
            decode(json!({"action":action,"directory":"../../checkout"}))?,
        )
        .await
        .map_err(|error| format!("{action}: {}", error.message().get().as_str()))?;
        assert_eq!(
            *outcome.data().get().effect().get(),
            WorkspaceWorktreeEffectDto::Completed
        );
    }
    assert_eq!(
        std::fs::read_to_string(source.join("file.txt"))?,
        "committed child"
    );
    assert!(!root.0.join("checkout").exists());
    Ok(())
}
