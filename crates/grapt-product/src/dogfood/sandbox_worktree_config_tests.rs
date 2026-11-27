use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Repository(PathBuf);
impl Drop for Repository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn git(root: &Path, args: &[&str]) -> TestResult {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
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
fn consent(path: &Path, yes: bool) -> TestResult<grapt_api::u3a::SandboxWorktreeConfigRequestDto> {
    Ok(serde_json::from_value(
        json!({"consent":yes,"path":path.to_str().ok_or("fixture path not UTF-8")?}),
    )?)
}

#[tokio::test]
async fn enabled_missing_worktree_config_is_read_only_until_explicit_consent() -> TestResult {
    let fixture = Repository(
        std::env::temp_dir().join(format!("grapt-worktree-config-{}", std::process::id())),
    );
    std::fs::create_dir(&fixture.0)?;
    git(&fixture.0, &["init"])?;
    assert!(missing(&fixture.0).await?.is_none());
    git(
        &fixture.0,
        &["config", "extensions.worktreeConfig", "false"],
    )?;
    assert!(missing(&fixture.0).await?.is_none());
    git(&fixture.0, &["config", "extensions.worktreeConfig", "true"])?;
    let path = missing(&fixture.0)
        .await?
        .ok_or("missing setting was not detected")?;
    assert!(!path.exists(), "detection must not create the file");
    assert!(create_missing(&fixture.0, consent(&path, false)?)
        .await
        .is_err());
    assert!(
        create_missing(&fixture.0, consent(&path.with_file_name("wrong"), true)?)
            .await
            .is_err()
    );
    assert!(!path.exists());
    create_missing(&fixture.0, consent(&path, true)?).await?;
    assert_eq!(std::fs::metadata(&path)?.len(), 0);
    assert!(missing(&fixture.0).await?.is_none());
    std::fs::write(&path, "[core]\n    logAllRefUpdates = true\n")?;
    assert!(create_missing(&fixture.0, consent(&path, true)?)
        .await
        .is_err());
    assert!(std::fs::read_to_string(&path)?.contains("logAllRefUpdates"));

    git(
        &fixture.0,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    )?;
    let linked = fixture.0.join("linked");
    git(
        &fixture.0,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().ok_or("fixture path")?,
        ],
    )?;
    // Git may copy an existing primary config.worktree when adding a worktree.
    // Establish the missing-file condition explicitly in this disposable fixture.
    let linked_config = fixture.0.join(".git/worktrees/linked/config.worktree");
    if linked_config.exists() {
        std::fs::remove_file(&linked_config)?;
    }
    let nested = linked.join("nested");
    std::fs::create_dir(&nested)?;
    let linked_path = missing(&nested)
        .await?
        .ok_or("linked missing file was not detected")?;
    assert_ne!(linked_path, path);
    assert!(linked_path.ends_with(
        Path::new("worktrees")
            .join("linked")
            .join("config.worktree")
    ));
    assert!(!linked_path.exists());
    create_missing(&nested, consent(&linked_path, true)?).await?;
    assert_eq!(std::fs::metadata(&linked_path)?.len(), 0);
    assert!(missing(&nested).await?.is_none());
    Ok(())
}
