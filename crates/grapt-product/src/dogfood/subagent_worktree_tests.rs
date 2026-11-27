use super::*;

fn git(root: &std::path::Path, args: &[&str]) -> TestResult {
    let output = std::process::Command::new("git")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .current_dir(root)
        .args([
            "-c",
            if cfg!(windows) {
                "core.hooksPath=NUL"
            } else {
                "core.hooksPath=/dev/null"
            },
        ])
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "fixture Git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

pub(super) fn commit_source(root: &std::path::Path) -> TestResult {
    std::fs::create_dir(root.join("nested"))?;
    std::fs::write(root.join("nested/file.txt"), "committed")?;
    git(root, &["add", "nested/file.txt"])?;
    git(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
    )
}

#[tokio::test]
async fn prepared_worktrees_preserve_parent_edits_nested_cwd_and_child_lineage() -> TestResult {
    let root = Root::new()?;
    commit_source(&root.0)?;
    std::fs::write(root.0.join("nested/file.txt"), "parent edit")?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let source = root.0.join("nested");
    let parent = a_parent(&app, source.to_str().ok_or("fixture path encoding")?)?;
    let agent = AgentId::new("agt_live_headless1").ok_or("agent")?;
    let mut directories = Vec::new();
    for _ in 0..2 {
        let options = ChildOptions::new(SubagentMode::Background)
            .with_worktree(&app, &parent)
            .await?;
        let child = admit_with_options(&app, &parent, &agent, "isolated task", None, &options)?;
        let record = app.get_record(&child.session)?;
        let directory = std::path::Path::new(record.placement.directory());
        assert_eq!(record.parent.as_ref(), Some(&parent));
        assert_eq!(record.preset.as_deref(), Some("author"));
        assert!(directory.ends_with("nested"));
        assert_eq!(
            std::fs::read_to_string(directory.join("file.txt"))?,
            "committed"
        );
        assert!(directory
            .parent()
            .ok_or("checkout root")?
            .join(".git")
            .is_file());
        assert!(directory.starts_with(std::fs::canonicalize(root.0.join("worktrees"))?));
        directories.push(directory.to_owned());
    }
    assert_ne!(directories[0], directories[1]);
    assert_eq!(
        std::fs::read_to_string(source.join("file.txt"))?,
        "parent edit"
    );
    Ok(())
}

#[tokio::test]
async fn a_prepared_checkout_cannot_be_admitted_for_another_parent() -> TestResult {
    let root = Root::new()?;
    commit_source(&root.0)?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let directory = root.0.to_str().ok_or("fixture path encoding")?;
    let parent = a_parent(&app, directory)?;
    let other = a_parent(&app, directory)?;
    let agent = AgentId::new("agt_live_headless1").ok_or("agent")?;
    let options = ChildOptions::new(SubagentMode::Background)
        .with_worktree(&app, &parent)
        .await?;
    let count = || -> Result<i64, rusqlite::Error> {
        app.lock_inbox()
            .store()
            .connection()
            .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))
    };
    let before = count()?;
    assert!(admit_with_options(&app, &other, &agent, "wrong parent", None, &options).is_err());
    assert_eq!(
        count()?,
        before,
        "refused placement created durable child state"
    );
    assert!(root.0.join("worktrees").is_dir());
    Ok(())
}

#[tokio::test]
async fn unborn_repository_and_blocked_destination_refuse_without_admission() -> TestResult {
    let root = Root::new()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let parent = a_parent(&app, root.0.to_str().ok_or("fixture path encoding")?)?;
    let count = || -> Result<i64, rusqlite::Error> {
        app.lock_inbox()
            .store()
            .connection()
            .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))
    };
    let before = count()?;
    assert!(ChildOptions::new(SubagentMode::Background)
        .with_worktree(&app, &parent)
        .await
        .is_err());
    assert_eq!(count()?, before);
    std::fs::remove_dir(root.0.join("worktrees"))?;
    std::fs::write(root.0.join("worktrees"), "keep this file")?;
    let error = ChildOptions::new(SubagentMode::Background)
        .with_worktree(&app, &parent)
        .await
        .err()
        .ok_or("a file was accepted as the worktree container")?;
    assert!(error.contains("real directory"), "{error}");
    assert_eq!(count()?, before);
    assert_eq!(
        std::fs::read_to_string(root.0.join("worktrees"))?,
        "keep this file"
    );
    Ok(())
}
