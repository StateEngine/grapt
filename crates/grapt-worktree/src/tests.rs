use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

mod core;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, std::io::Error> {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("grapt-worktree-{}-{sequence}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

async fn repository(root: &Path) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let source = root.join("source repo");
    std::fs::create_dir(&source)?;
    git_output(&source, &["init", "--initial-branch=main"], false).await?;
    std::fs::create_dir(source.join("nested dir"))?;
    std::fs::write(source.join("nested dir/file.txt"), "committed\n")?;
    std::fs::write(
        source.join(".gitattributes"),
        "*.txt -text filter=specimen\n",
    )?;
    git_output(&source, &["add", "."], false).await?;
    git_output(
        &source,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "initial",
        ],
        false,
    )
    .await?;
    Ok(source)
}

#[tokio::test]
async fn detached_checkout_preserves_nested_cwd_and_leaves_source_changes_alone() -> TestResult {
    let scratch = Scratch::new()?;
    let source = repository(&scratch.0).await?;
    std::fs::write(source.join("nested dir/file.txt"), "local changes\n")?;
    let destination = scratch.0.join("new checkout");
    let cwd = create(&source.join("nested dir"), &destination).await?;
    assert_eq!(
        std::fs::canonicalize(&cwd)?,
        std::fs::canonicalize(destination.join("nested dir"))?
    );
    #[cfg(windows)]
    assert_eq!(
        std::fs::canonicalize(cwd.to_str().ok_or("path encoding")?.replace('\\', "/"))?,
        std::fs::canonicalize(&cwd)?,
        "ordinary Session separators must still name the created worktree"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("file.txt"))?,
        "committed\n"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("nested dir/file.txt"))?,
        "local changes\n"
    );
    assert_eq!(
        git_output(&destination, &["rev-parse", "--abbrev-ref", "HEAD"], false).await?,
        b"HEAD\n"
    );
    std::fs::write(cwd.join("file.txt"), "isolated\n")?;
    assert_eq!(
        std::fs::read_to_string(source.join("nested dir/file.txt"))?,
        "local changes\n"
    );
    Ok(())
}

#[tokio::test]
async fn existing_destination_and_nonrepository_are_refused_without_overwrite() -> TestResult {
    let scratch = Scratch::new()?;
    let source = repository(&scratch.0).await?;
    let destination = scratch.0.join("existing");
    std::fs::create_dir(&destination)?;
    std::fs::write(destination.join("keep"), "user data")?;
    assert!(create(&source, &destination).await.is_err());
    assert_eq!(
        std::fs::read_to_string(destination.join("keep"))?,
        "user data"
    );
    let missing = scratch.0.join("not-created");
    assert!(create(&scratch.0, &missing).await.is_err());
    assert!(!missing.exists());
    Ok(())
}

#[tokio::test]
async fn untracked_nested_directory_fails_and_preserves_checkout_for_inspection() -> TestResult {
    let scratch = Scratch::new()?;
    let source = repository(&scratch.0).await?;
    std::fs::create_dir(source.join("untracked"))?;
    let destination = scratch.0.join("preserved");
    let error = create(&source.join("untracked"), &destination)
        .await
        .err()
        .ok_or("expected refusal")?;
    assert!(error.contains("preserved"), "{error}");
    assert!(destination.join(".git").is_file());
    assert!(source.join("untracked").is_dir());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn checkout_disables_hooks_and_filters() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new()?;
    let source = repository(&scratch.0).await?;
    let hooks = scratch.0.join("hooks");
    std::fs::create_dir(&hooks)?;
    std::fs::write(hooks.join("post-checkout"), "#!/bin/sh\nexit 91\n")?;
    std::fs::set_permissions(
        hooks.join("post-checkout"),
        std::fs::Permissions::from_mode(0o700),
    )?;
    git_output(
        &source,
        &[
            OsStr::new("config"),
            OsStr::new("core.hooksPath"),
            hooks.as_os_str(),
        ],
        false,
    )
    .await?;
    git_output(
        &source,
        &["config", "filter.specimen.smudge", "exit 92"],
        false,
    )
    .await?;
    git_output(
        &source,
        &["config", "filter.specimen.required", "true"],
        false,
    )
    .await?;
    let cwd = create(&source, &scratch.0.join("no-helpers")).await?;
    assert_eq!(
        std::fs::read_to_string(cwd.join("nested dir/file.txt"))?,
        "committed\n"
    );
    // Shared repository configuration is preserved.
    assert_eq!(
        git_output(&source, &["config", "filter.specimen.required"], false).await?,
        b"true\n"
    );
    Ok(())
}

#[tokio::test]
async fn source_core_worktree_setting_cannot_redirect_destination_reset() -> TestResult {
    let scratch = Scratch::new()?;
    let source = repository(&scratch.0).await?;
    git_output(
        &source,
        &[
            OsStr::new("config"),
            OsStr::new("core.worktree"),
            source.as_os_str(),
        ],
        false,
    )
    .await?;
    std::fs::write(source.join("nested dir/file.txt"), "keep local changes\n")?;
    let cwd = create(&source, &scratch.0.join("pinned-destination")).await?;
    assert_eq!(
        std::fs::read_to_string(source.join("nested dir/file.txt"))?,
        "keep local changes\n"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("nested dir/file.txt"))?,
        "committed\n"
    );
    Ok(())
}
