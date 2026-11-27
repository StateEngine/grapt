use std::path::PathBuf;

use crate::infra::*;

#[test]
fn a_missing_root_refuses_root_missing() {
    let scene = Scene::fresh("r01", false);
    let source = FileSource::new(scene.root.clone());
    let request = ActivationRequest {
        root: scene.root.join("no-such-root"),
        pin: PathBuf::from(PIN_REL),
        target: must_target(),
        platform: PLATFORM.to_owned(),
        arch: ARCH.to_owned(),
        clock: &scene.clock,
        source: &source,
    };
    expect_reason(
        &refused(activate(&request)),
        RefusalReason::RootMissing,
        "missing root",
    );
    scene.cleanup();
}

#[test]
fn an_uncreatable_tool_tree_refuses_root_unreadable() {
    let scene = Scene::fresh("r02", true);
    install(&scene, "15.1.0", b"fake-ripgrep-binary-v1");
    // Block the tool directory with a file: the tree cannot be created.
    let tool_dir = LAYOUT.tool_dir(&scene.root, "ripgrep");
    std::fs::remove_dir_all(&tool_dir).unwrap_or_else(|e| panic!("corpus: {e}"));
    std::fs::write(&tool_dir, b"blocker").unwrap_or_else(|e| panic!("corpus: {e}"));

    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::RootUnreadable,
        "blocked tool tree",
    );
    scene.cleanup();
}

#[test]
fn a_missing_pin_refuses_pin_missing() {
    // The pin check precedes attribution, so a non-git root isolates the fact.
    let scene = Scene::fresh("r03", false);
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::PinMissing,
        "missing pin",
    );
    scene.cleanup();
}

#[test]
fn a_non_regular_pin_refuses_pin_unreadable() {
    let scene = Scene::fresh("r04", false);
    std::fs::create_dir_all(scene.root.join(PIN_REL)).unwrap_or_else(|e| panic!("corpus: {e}"));
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::PinUnreadable,
        "directory where the pin stands",
    );
    scene.cleanup();
}

#[test]
fn an_oversized_pin_refuses_pin_oversized() {
    let scene = Scene::fresh("r05", false);
    let oversized = vec![b'{'; 65_537]; // the 64 KiB bound plus one
    scene.write(PIN_REL, oversized);
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::PinOversized,
        "oversized pin",
    );
    scene.cleanup();
}

#[test]
fn a_malformed_pin_refuses_pin_malformed() {
    // A foreign schema, committed.
    let scene = Scene::fresh("r06a", true);
    scene.write(
        PIN_REL,
        br#"{"schema":"grapt.toolchain.pins.v9","tool":"ripgrep","version":"15.1.0","pins":{}}"#
            .to_vec(),
    );
    scene.commit(&[PIN_REL]);
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::PinMalformed,
        "foreign schema",
    );
    scene.cleanup();

    // A duplicate target key: the strict decode sees both occurrences.
    let scene = Scene::fresh("r06b", true);
    let digest = Sha256::of(b"archive").hex();
    let twin = &[
        (TARGET, digest.as_str(), ENTRY, SOURCE_REL),
        (TARGET, digest.as_str(), ENTRY, SOURCE_REL),
    ];
    scene.write(PIN_REL, pin_text("15.1.0", twin));
    scene.commit(&[PIN_REL]);
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::PinMalformed,
        "duplicate target key",
    );
    scene.cleanup();
}

#[test]
fn a_non_git_root_refuses_pin_source_not_git() {
    let scene = Scene::fresh("r07", false);
    // The evidence requires a root that is genuinely outside any worktree;
    // if the temp dir sits inside one, fail loudly instead of claiming a
    // witness the environment cannot produce.
    let inside = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(&scene.root)
        .output()
        .unwrap_or_else(|error| panic!("corpus: git is unavailable: {error}"));
    assert!(
        !inside.status.success(),
        "corpus: temp root is inside a git worktree; the not-git evidence cannot be produced"
    );
    let digest = Sha256::of(b"archive").hex();
    scene.write(
        PIN_REL,
        pin_text("15.1.0", &[(TARGET, &digest, ENTRY, SOURCE_REL)]),
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::PinSourceNotGit,
        "non-git root",
    );
    scene.cleanup();
}

#[test]
fn an_untracked_pin_refuses_pin_source_untracked() {
    // A worktree with a HEAD commit, pin present but never committed.
    let scene = Scene::fresh("r08a", true);
    git(
        &scene.root,
        &["commit", "-q", "--allow-empty", "-m", "base"],
    );
    let digest = Sha256::of(b"archive").hex();
    scene.write(
        PIN_REL,
        pin_text("15.1.0", &[(TARGET, &digest, ENTRY, SOURCE_REL)]),
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::PinSourceUntracked,
        "uncommitted pin",
    );
    scene.cleanup();

    // A worktree with no HEAD commit at all: the same admissibility fact.
    let scene = Scene::fresh("r08b", true);
    scene.write(
        PIN_REL,
        pin_text("15.1.0", &[(TARGET, &digest, ENTRY, SOURCE_REL)]),
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::PinSourceUntracked,
        "pin without any HEAD commit",
    );
    scene.cleanup();
}

#[test]
fn a_dirty_pin_refuses_pin_source_dirty() {
    let scene = Scene::fresh("r09", true);
    let digest = Sha256::of(b"archive").hex();
    scene.write(
        PIN_REL,
        pin_text("15.1.0", &[(TARGET, &digest, ENTRY, SOURCE_REL)]),
    );
    scene.commit(&[PIN_REL]);
    // An uncommitted re-cut: the on-disk bytes differ from the HEAD blob.
    scene.write(
        PIN_REL,
        pin_text("15.1.0", &[(OTHER_TARGET, &digest, ENTRY, SOURCE_REL)]),
    );
    let source = FileSource::new(scene.root.clone());
    expect_reason(
        &refused(scene.run(&source)),
        RefusalReason::PinSourceDirty,
        "uncommitted re-cut",
    );
    scene.cleanup();
}

#[test]
fn an_absent_target_refuses_target_unsupported() {
    let archive = archive_stream(&[file_entry(ENTRY, b"fake-ripgrep-binary-v1")]);
    let digest = Sha256::of(&archive).hex();
    let scene = scene_with_input(
        "r10",
        "15.1.0",
        &[(OTHER_TARGET, &digest, ENTRY, SOURCE_REL)],
        archive,
    );
    let source = FileSource::new(scene.root.clone());
    let refusal = refused(scene.run(&source));
    expect_reason(&refusal, RefusalReason::TargetUnsupported, "absent target");
    assert_eq!(refusal.context.tool.as_deref(), Some("ripgrep"));
    assert_eq!(refusal.context.version.as_deref(), Some("15.1.0"));
    assert_eq!(refusal.context.target.as_deref(), Some(TARGET));
    scene.cleanup();
}
