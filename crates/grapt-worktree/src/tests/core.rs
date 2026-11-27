use super::*;

#[tokio::test]
async fn ancestor_worktree_redirect_cannot_change_the_selected_source_repository() -> TestResult {
    let scratch = Scratch::new()?;
    let source = repository(&scratch.0).await?;
    let nested_checkout = source.join("nested checkout");
    let other = scratch.0.join("other checkout");
    create(&source, &nested_checkout).await?;
    create(&source, &other).await?;
    git_output(
        &source,
        &["config", "extensions.worktreeConfig", "true"],
        false,
    )
    .await?;
    git_output(
        &nested_checkout,
        &[
            OsStr::new("config"),
            OsStr::new("--worktree"),
            OsStr::new("core.worktree"),
            source.as_os_str(),
        ],
        false,
    )
    .await?;
    assert_eq!(
        std::fs::canonicalize(
            git_path(&nested_checkout, &["rev-parse", "--show-toplevel"]).await?
        )?,
        std::fs::canonicalize(&source)?,
        "the adversarial worktree-specific override must redirect Git to the ancestor"
    );
    let destination = scratch.0.join("must not allocate");
    assert!(create(&nested_checkout, &destination).await.is_err());
    assert!(
        !destination.exists(),
        "redirected source must not allocate a checkout in the ancestor repository"
    );
    assert!(remove(&nested_checkout, &other).await.is_err());
    assert!(other.join(".git").is_file());
    Ok(())
}

async fn commit(root: &Path) -> TestResult {
    git_output(root, &["add", "."], true).await?;
    git_output(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "work",
        ],
        false,
    )
    .await?;
    Ok(())
}

#[tokio::test]
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn remove_prunes_empty_scaffold_but_preserves_retained_files() -> TestResult {
    let scratch = Scratch::new()?;
    let source = repository(&scratch.0).await?;
    std::fs::write(
        source.join(".git/info/exclude"),
        "target/\nnested dir/file.txt\n",
    )?;
    std::fs::create_dir(source.join("target"))?;
    std::fs::write(source.join("target/copied"), "setup content\n")?;
    let checkout = scratch.0.join("empty scaffold");
    create_with_setup(&source, &checkout, &[PathBuf::from("target/copied")]).await?;
    std::fs::remove_file(checkout.join("target/copied"))?;
    std::fs::create_dir_all(checkout.join("target/empty/nested"))?;
    std::fs::create_dir_all(checkout.join("untracked scaffold/nested"))?;
    remove(&source, &checkout).await?;
    assert!(!checkout.exists());

    let retained = scratch.0.join("retained content");
    create_with_setup(&source, &retained, &[PathBuf::from("target/copied")]).await?;
    std::fs::create_dir_all(retained.join("target/empty/nested"))?;
    std::fs::create_dir_all(retained.join("untracked scaffold/nested"))?;
    let error = remove(&source, &retained)
        .await
        .err()
        .ok_or("expected retained ignored file refusal")?;
    assert_eq!(error.effect(), WorktreeEffect::NotStarted);
    assert!(
        error.message().contains("retained ignored files"),
        "{error}"
    );
    assert!(error.message().contains("target/copied"), "{error}");
    assert_eq!(
        std::fs::read_to_string(retained.join("target/copied"))?,
        "setup content\n"
    );
    assert!(retained.join("target/empty/nested").is_dir());
    assert!(retained.join("untracked scaffold/nested").is_dir());
    std::fs::remove_file(retained.join("target/copied"))?;

    std::fs::write(retained.join("untracked-content"), "local content\n")?;
    let error = remove(&source, &retained)
        .await
        .err()
        .ok_or("expected uncommitted file refusal")?;
    assert_eq!(error.effect(), WorktreeEffect::NotStarted);
    assert!(error.message().contains("uncommitted changes"), "{error}");
    assert!(error.message().contains("untracked-content"), "{error}");
    assert_eq!(
        std::fs::read_to_string(retained.join("untracked-content"))?,
        "local content\n"
    );
    assert!(retained.join("target/empty/nested").is_dir());
    std::fs::remove_file(retained.join("untracked-content"))?;

    std::fs::write(
        retained.join("nested dir/file.txt"),
        "tracked local edits\n",
    )?;
    let error = remove(&source, &retained)
        .await
        .err()
        .ok_or("expected dirty tracked file refusal")?;
    assert_eq!(error.effect(), WorktreeEffect::NotStarted);
    assert!(error.message().contains("uncommitted changes"), "{error}");
    assert!(error.message().contains("nested dir/file.txt"), "{error}");
    assert!(retained.join("target/empty/nested").is_dir());
    std::fs::write(retained.join("nested dir/file.txt"), "committed\n")?;
    for (directory, category) in [
        ("target/.git", "retained ignored files"),
        ("unknown/.git", "uncommitted changes"),
    ] {
        let metadata = retained.join(directory);
        std::fs::create_dir_all(&metadata)?;
        std::fs::write(metadata.join("keep"), "nested metadata content\n")?;
        let error = remove(&source, &retained)
            .await
            .err()
            .ok_or("expected nested .git content refusal")?;
        assert_eq!(error.effect(), WorktreeEffect::NotStarted);
        assert!(error.message().contains(category), "{error}");
        assert!(error.message().contains(".git/keep"), "{error}");
        assert_eq!(
            std::fs::read_to_string(metadata.join("keep"))?,
            "nested metadata content\n"
        );
        assert!(retained.join("target/empty/nested").is_dir());
        assert!(retained.join("untracked scaffold/nested").is_dir());
        std::fs::remove_file(metadata.join("keep"))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt as _;
        for (name, category, tracked) in [
            ("target/fifo", "retained ignored files", false),
            ("nested dir/file.txt", "uncommitted changes", true),
        ] {
            let fifo = retained.join(name);
            if tracked {
                std::fs::remove_file(&fifo)?;
            }
            assert!(
                fifo.is_absolute(),
                "mkfifo requires an absolute fixture path"
            );
            let status = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .map_err(|error| {
                    format!("mkfifo is required for the special-file fixture: {error}")
                })?;
            assert!(
                status.success(),
                "mkfifo failed for {}: {status}",
                fifo.display()
            );
            assert!(std::fs::symlink_metadata(&fifo)?.file_type().is_fifo());
            let error = remove(&source, &retained)
                .await
                .err()
                .ok_or("expected special-file refusal")?;
            assert_eq!(error.effect(), WorktreeEffect::NotStarted);
            assert!(error.message().contains(category), "{error}");
            assert!(error.message().contains(name), "{error}");
            assert!(std::fs::symlink_metadata(&fifo)?.file_type().is_fifo());
            assert!(retained.join("target/empty/nested").is_dir());
            std::fs::remove_file(&fifo)?;
            if tracked {
                std::fs::write(&fifo, "committed\n")?;
            }
        }
    }
    remove(&source, &retained).await?;
    assert!(!retained.exists());
    assert_eq!(
        std::fs::read_to_string(source.join("target/copied"))?,
        "setup content\n"
    );
    Ok(())
}

#[tokio::test]
async fn setup_copies_explicit_root_relative_files_and_refuses_unsafe_allowlists() -> TestResult {
    let scratch = Scratch::new()?;
    let source = repository(&scratch.0).await?;
    std::fs::write(source.join(".git/info/exclude"), ".env\n")?;
    std::fs::write(source.join(".env"), "local environment\n")?;
    std::fs::write(source.join("nested dir/local"), "local configuration\n")?;
    let destination = scratch.0.join("setup checkout");
    let cwd = create_with_setup(
        &source.join("nested dir"),
        &destination,
        &[PathBuf::from(".env"), PathBuf::from("nested dir/local")],
    )
    .await?;
    assert_eq!(
        std::fs::read_to_string(destination.join(".env"))?,
        "local environment\n"
    );
    assert_eq!(
        std::fs::read_to_string(cwd.join("local"))?,
        "local configuration\n"
    );
    for invalid in [
        "../outside",
        ".git/config",
        ".GIT/config",
        ".git./config",
        "file:stream",
        "NUL",
        "nested dir/file.txt",
    ] {
        let refused = scratch.0.join("not created");
        let error = create_with_setup(&source, &refused, &[PathBuf::from(invalid)])
            .await
            .err()
            .ok_or("expected setup preflight refusal")?;
        assert_eq!(error.effect(), WorktreeEffect::NotStarted, "{invalid}");
        assert!(!error.may_have_changed());
        assert_eq!(error.to_string(), error.message());
        assert!(!refused.exists());
    }
    assert!(create_with_setup(
        &source,
        &scratch.0.join("duplicate"),
        &[PathBuf::from(".env"), PathBuf::from(".env")]
    )
    .await
    .is_err());
    std::fs::create_dir(source.join("untracked cwd"))?;
    let preserved = scratch.0.join("preserved missing cwd");
    let error = create_with_setup(
        &source.join("untracked cwd"),
        &preserved,
        &[PathBuf::from(".env")],
    )
    .await
    .err()
    .ok_or("expected post-create cwd refusal")?;
    assert_eq!(error.effect(), WorktreeEffect::MayHaveChanged);
    assert!(error.may_have_changed());
    assert!(preserved.join(".git").is_file());
    std::fs::write(source.join("oversized"), vec![0; 1024 * 1024 + 1])?;
    assert!(create_with_setup(
        &source,
        &scratch.0.join("large"),
        &[PathBuf::from("oversized")]
    )
    .await
    .is_err());
    assert_eq!(
        std::fs::read_to_string(source.join(".env"))?,
        "local environment\n"
    );
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn merge_and_remove_preserve_local_and_unmerged_work_before_fast_forward() -> TestResult {
    let scratch = Scratch::new()?;
    let source = repository(&scratch.0).await?;
    let checkout = scratch.0.join("linked");
    create(&source, &checkout).await?;
    std::fs::write(checkout.join("nested dir/file.txt"), "committed work\n")?;
    let error = merge_back(&source, &checkout)
        .await
        .err()
        .ok_or("expected dirty merge refusal")?;
    assert_eq!(error.effect(), WorktreeEffect::NotStarted);
    let error = remove(&source, &checkout)
        .await
        .err()
        .ok_or("expected dirty removal refusal")?;
    assert!(!error.may_have_changed());
    commit(&checkout).await?;
    assert!(remove(&source.join("nested dir"), &checkout).await.is_err());
    std::fs::write(source.join("nested dir/file.txt"), "source edits\n")?;
    assert!(merge_back(&source.join("nested dir"), &checkout)
        .await
        .is_err());
    assert_eq!(
        std::fs::read_to_string(source.join("nested dir/file.txt"))?,
        "source edits\n"
    );
    std::fs::write(source.join("nested dir/file.txt"), "committed\n")?;
    std::fs::write(checkout.join("untracked"), "keep\n")?;
    assert!(merge_back(&source, &checkout).await.is_err());
    assert!(remove(&source, &checkout).await.is_err());
    std::fs::remove_file(checkout.join("untracked"))?;
    git_output(
        &source,
        &["config", "--", "branch.main.mergeOptions", "--squash"],
        false,
    )
    .await?;
    merge_back(&source.join("nested dir"), &checkout).await?;
    assert_eq!(
        git_output(&source, &["rev-parse", "HEAD"], false).await?,
        git_output(&checkout, &["rev-parse", "HEAD"], false).await?,
        "fast-forward must advance HEAD even when branch defaults request squash"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("nested dir/file.txt"))?,
        "committed work\n"
    );
    std::fs::write(source.join(".git/info/exclude"), "ignored\n")?;
    std::fs::write(checkout.join("ignored"), "do not delete\n")?;
    assert!(remove(&source, &checkout).await.is_err());
    assert_eq!(
        std::fs::read_to_string(checkout.join("ignored"))?,
        "do not delete\n"
    );
    std::fs::remove_file(checkout.join("ignored"))?;
    for (flag, clear) in [
        ("--assume-unchanged", "--no-assume-unchanged"),
        ("--skip-worktree", "--no-skip-worktree"),
    ] {
        git_output(
            &checkout,
            &["update-index", flag, "--", "nested dir/file.txt"],
            false,
        )
        .await?;
        std::fs::write(checkout.join("nested dir/file.txt"), "hidden local edits\n")?;
        assert!(remove(&source, &checkout).await.is_err(), "{flag}");
        assert!(merge_back(&source, &checkout).await.is_err(), "{flag}");
        assert_eq!(
            std::fs::read_to_string(checkout.join("nested dir/file.txt"))?,
            "hidden local edits\n"
        );
        git_output(
            &checkout,
            &["update-index", clear, "--", "nested dir/file.txt"],
            false,
        )
        .await?;
        std::fs::write(checkout.join("nested dir/file.txt"), "committed work\n")?;
    }
    assert!(remove(&checkout, &source).await.is_err());
    assert!(remove(&source, &source).await.is_err());
    git_output(
        &source,
        &[
            OsStr::new("worktree"),
            OsStr::new("lock"),
            OsStr::new("--"),
            checkout.as_os_str(),
        ],
        false,
    )
    .await?;
    let error = remove(&source, &checkout)
        .await
        .err()
        .ok_or("expected locked removal refusal")?;
    assert_eq!(error.effect(), WorktreeEffect::MayHaveChanged);
    assert!(checkout.join(".git").is_file());
    git_output(
        &source,
        &[
            OsStr::new("worktree"),
            OsStr::new("unlock"),
            OsStr::new("--"),
            checkout.as_os_str(),
        ],
        false,
    )
    .await?;
    remove(&source.join("nested dir"), &checkout).await?;
    assert!(!checkout.exists());

    let divergent = scratch.0.join("divergent");
    create(&source, &divergent).await?;
    std::fs::write(source.join("source-only"), "source branch\n")?;
    commit(&source).await?;
    std::fs::write(divergent.join("checkout-only"), "unmerged branch\n")?;
    commit(&divergent).await?;
    let source_head = git_output(&source, &["rev-parse", "HEAD"], false).await?;
    assert!(merge_back(&source, &divergent).await.is_err());
    assert!(remove(&source, &divergent).await.is_err());
    assert_eq!(
        git_output(&source, &["rev-parse", "HEAD"], false).await?,
        source_head
    );
    assert!(divergent.join("checkout-only").is_file());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn setup_refuses_source_links_and_redirected_checkout_parents_without_outside_writes(
) -> TestResult {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new()?;
    let source = repository(&scratch.0).await?;
    let outside = scratch.0.join("outside");
    std::fs::create_dir(&outside)?;
    std::fs::write(outside.join("existing"), "outside data\n")?;
    symlink(outside.join("existing"), source.join("linked-file"))?;
    assert!(create_with_setup(
        &source,
        &scratch.0.join("bad-source"),
        &[PathBuf::from("linked-file")]
    )
    .await
    .is_err());
    symlink(&outside, source.join("redirect"))?;
    git_output(&source, &["add", "redirect"], false).await?;
    git_output(
        &source,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "redirect",
        ],
        false,
    )
    .await?;
    std::fs::remove_file(source.join("redirect"))?;
    std::fs::create_dir(source.join("redirect"))?;
    std::fs::write(source.join("redirect/local"), "setup data\n")?;
    let destination = scratch.0.join("preserved");
    let error = create_with_setup(&source, &destination, &[PathBuf::from("redirect/local")])
        .await
        .err()
        .ok_or("expected redirected parent refusal")?;
    assert!(error.message().contains("preserved"), "{error}");
    assert_eq!(error.effect(), WorktreeEffect::MayHaveChanged);
    assert!(destination.join(".git").is_file());
    assert!(!outside.join("local").exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("existing"))?,
        "outside data\n"
    );
    Ok(())
}
