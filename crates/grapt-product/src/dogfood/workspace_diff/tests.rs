use super::*;
use std::fs;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
struct Fixture {
    base: std::path::PathBuf,
    repo: std::path::PathBuf,
}
impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let base = std::env::temp_dir().join(format!(
            "grapt-workspace-diff-{label}-{}",
            std::process::id()
        ));
        if base.exists() {
            fs::remove_dir_all(&base)?;
        }
        let repo = base.join("repo");
        fs::create_dir_all(&repo)?;
        let fixture = Self { base, repo };
        fixture.git(&["init", "-q"])?;
        fixture.git(&["config", "user.name", "Grapt test"])?;
        fixture.git(&["config", "user.email", "test@grapt.invalid"])?;
        fs::write(fixture.repo.join("tracked.txt"), "before\n")?;
        fixture.git(&["add", "--", "tracked.txt"])?;
        fixture.git(&["commit", "-qm", "seed"])?;
        Ok(fixture)
    }
    fn git(&self, args: &[&str]) -> TestResult {
        let output = std::process::Command::new("git")
            .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err(format!(
                "fixture git failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )
            .into());
        }
        Ok(())
    }
    async fn read(&self) -> TestResult<(bool, String)> {
        Ok(tokio::time::timeout(DIFF_DEADLINE, snapshot(&self.repo)).await??)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[tokio::test]
async fn staged_unstaged_and_untracked_changes_are_read_without_rewriting_the_index() -> TestResult
{
    let fixture = Fixture::new("all-kinds")?;
    fs::write(fixture.repo.join("tracked.txt"), "staged version\n")?;
    fixture.git(&["add", "--", "tracked.txt"])?;
    fs::write(fixture.repo.join("tracked.txt"), "working version\n")?;
    fs::write(fixture.repo.join("a new file.txt"), "untracked contents\n")?;
    let before = fs::read(fixture.repo.join(".git/index"))?;
    let (is_git, diff) = fixture.read().await?;
    assert!(is_git);
    for expected in [
        "STAGED",
        "UNSTAGED",
        "UNTRACKED",
        "+staged version",
        "+working version",
        "+untracked contents",
        "a new file.txt",
    ] {
        assert!(diff.contains(expected), "missing {expected}: {diff}");
    }
    assert_eq!(fs::read(fixture.repo.join(".git/index"))?, before);
    Ok(())
}

#[tokio::test]
async fn clean_absent_and_corrupt_git_are_distinct() -> TestResult {
    let fixture = Fixture::new("states")?;
    assert_eq!(fixture.read().await?, (true, String::new()));
    assert_eq!(snapshot(&fixture.base).await?, (false, String::new()));
    fs::write(fixture.repo.join(".git/HEAD"), "not a Git ref\n")?;
    assert!(
        snapshot(&fixture.repo).await.is_err(),
        "corrupt metadata is not a non-repository"
    );
    Ok(())
}

#[tokio::test]
async fn unborn_repositories_show_staged_and_untracked_files() -> TestResult {
    let fixture = Fixture::new("unborn")?;
    fs::remove_dir_all(fixture.repo.join(".git"))?;
    fixture.git(&["init", "-q"])?;
    fixture.git(&["add", "--", "tracked.txt"])?;
    fs::write(fixture.repo.join("new.txt"), "new before first commit\n")?;
    let (_, diff) = fixture.read().await?;
    assert!(
        diff.contains("STAGED") && diff.contains("+before"),
        "{diff}"
    );
    assert!(
        diff.contains("UNTRACKED") && diff.contains("+new before first commit"),
        "{diff}"
    );
    Ok(())
}

#[tokio::test]
async fn oversized_diffs_fail_explicitly_instead_of_looking_complete() -> TestResult {
    let fixture = Fixture::new("byte-bound")?;
    fs::write(fixture.repo.join("large.txt"), "x\n".repeat(MAX_DIFF_BYTES))?;
    let error = fixture
        .read()
        .await
        .err()
        .ok_or("oversized diff passed")?
        .to_string();
    assert!(error.contains("128 KiB"), "{error}");
    Ok(())
}

#[test]
fn too_many_untracked_paths_fail_with_the_file_bound() -> TestResult {
    let mut paths = Vec::new();
    for index in 0..MAX_UNTRACKED_FILES {
        paths.extend_from_slice(format!("file-{index:03}.txt\0").as_bytes());
    }
    assert_eq!(untracked_paths(&paths)?.count(), MAX_UNTRACKED_FILES);
    paths.extend_from_slice(b"overflow.txt\0");
    let error = untracked_paths(&paths).err().ok_or("file bound passed")?;
    assert!(error.contains("128 untracked files"), "{error}");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn diff_never_runs_repository_filters_textconv_fsmonitor_or_hooks() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("helpers")?;
    let marker = fixture.base.join("helper-ran");
    let helper = fixture.base.join("helper");
    let quoted_marker = marker.to_string_lossy().replace('\'', "'\\''");
    fs::write(
        &helper,
        format!("#!/bin/sh\nprintf invoked > '{quoted_marker}'\nexit 1\n"),
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
    let helper = helper.to_str().ok_or("helper path")?;
    let hooks = fixture.base.join("hooks");
    fs::create_dir(&hooks)?;
    fs::copy(helper, hooks.join("post-index-change"))?;
    fs::set_permissions(
        hooks.join("post-index-change"),
        fs::Permissions::from_mode(0o755),
    )?;
    for key in [
        "core.fsmonitor",
        "diff.external",
        "diff.evil.command",
        "diff.evil.textconv",
        "filter.x=y.clean",
        "filter.x=y.process",
    ] {
        fixture.git(&["config", key, helper])?;
    }
    fixture.git(&["config", "filter.x=y.required", "true"])?;
    fixture.git(&[
        "config",
        "core.hooksPath",
        hooks.to_str().ok_or("hooks path")?,
    ])?;
    fs::write(
        fixture.repo.join(".gitattributes"),
        "*.txt filter=x=y diff=evil\n",
    )?;
    fs::write(fixture.repo.join("tracked.txt"), "modified contents\n")?;
    fs::write(fixture.repo.join("new.txt"), "new contents\n")?;
    let (_, diff) = fixture.read().await?;
    assert!(!marker.exists(), "a repository-selected executable ran");
    assert!(
        diff.contains("+modified contents") && diff.contains("+new contents"),
        "{diff}"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn untracked_symlinks_show_the_link_without_reading_its_target() -> TestResult {
    let fixture = Fixture::new("symlink")?;
    fs::write(
        fixture.base.join("outside"),
        "DO_NOT_READ_OUTSIDE_CONTENTS\n",
    )?;
    std::os::unix::fs::symlink("../outside", fixture.repo.join("link"))?;
    let (_, diff) = fixture.read().await?;
    assert!(diff.contains("+../outside"), "{diff}");
    assert!(!diff.contains("DO_NOT_READ_OUTSIDE_CONTENTS"));
    Ok(())
}

#[tokio::test]
async fn global_ignore_rules_survive_the_cleared_git_environment() -> TestResult {
    let fixture = Fixture::new("global-ignore")?;
    let config = fixture.base.join("global.config");
    let ignores = fixture.base.join("ignore-patterns");
    let ignore_path = serde_json::to_string(ignores.to_str().ok_or("ignore path")?)?;
    fs::write(&config, format!("[core]\n excludesFile = {ignore_path}\n"))?;
    fs::write(&ignores, "ignore-me.txt\n")?;
    fs::write(
        fixture.repo.join("ignore-me.txt"),
        "GLOBAL_IGNORE_SENTINEL\n",
    )?;
    fs::write(fixture.repo.join("visible.txt"), "visible contents\n")?;
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "dogfood::workspace_diff::tests::global_ignore_child",
            "--ignored",
        ])
        .env("GRAPT_DIFF_TEST_ROOT", &fixture.repo)
        .env("GIT_CONFIG_GLOBAL", &config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let group = processkit::ProcessGroup::new()?;
    let mut child = group.spawn(command)?;
    let out = child.stdout.take().ok_or("fixture stdout")?;
    let err = child.stderr.take().ok_or("fixture stderr")?;
    let (out, err, status) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::try_join!(read(out, 16 * 1024), read(err, 16 * 1024), async {
            child.wait().await.map_err(|error| error.to_string())
        })
    })
    .await??;
    assert!(
        status.success(),
        "global-ignore child failed: {} {}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    Ok(())
}

#[tokio::test]
#[ignore = "runs only as the isolated child of global_ignore_rules_survive_the_cleared_git_environment"]
async fn global_ignore_child() -> TestResult {
    let root =
        std::env::var_os("GRAPT_DIFF_TEST_ROOT").ok_or("global-ignore child root missing")?;
    let (_, diff) = tokio::time::timeout(DIFF_DEADLINE, snapshot(Path::new(&root))).await??;
    assert!(diff.contains("visible contents"), "{diff}");
    assert!(
        !diff.contains("ignore-me.txt") && !diff.contains("GLOBAL_IGNORE_SENTINEL"),
        "{diff}"
    );
    Ok(())
}
