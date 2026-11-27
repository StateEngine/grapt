use std::path::Path;

use crate::infra::*;

#[test]
fn a_held_lock_refuses_activation_in_flight() {
    let scene = Scene::fresh("r18", true);
    install(&scene, "15.1.0", b"fake-ripgrep-binary-v1");
    // A lock file naming its holder process: the boundary never guesses
    // liveness, it names the holder and the operator confirms.
    std::fs::create_dir_all(LAYOUT.tool_dir(&scene.root, "ripgrep"))
        .unwrap_or_else(|e| panic!("corpus: {e}"));
    let lock_text = serde_json::json!({
        "schema": "grapt.toolchain.lock.v1",
        "holder_pid": 4242,
        "acquired_at_ms": 1
    })
    .to_string();
    std::fs::write(LAYOUT.lock(&scene.root, "ripgrep"), lock_text)
        .unwrap_or_else(|e| panic!("corpus: {e}"));

    let source = FileSource::new(scene.root.clone());
    let refusal = refused(scene.run(&source));
    expect_reason(&refusal, RefusalReason::ActivationInFlight, "held lock");
    assert!(
        refusal.detail.contains("4242"),
        "corpus: the refusal must name the holder process: {refusal}"
    );

    // The previous activation stands untouched: the refusal precedes any write.
    assert_eq!(
        std::fs::read(LAYOUT.canonical(&scene.root, "ripgrep", ENTRY)).unwrap_or_default(),
        b"fake-ripgrep-binary-v1"
    );
    scene.cleanup();
}

#[test]
fn a_publish_failure_refuses_activation_failed_and_leaves_no_canonical() {
    // A directory standing where the canonical executable must be: the
    // staged-to-canonical rename cannot take, and the previous state does
    // not exist to restore.
    let scene = Scene::fresh("r20", true);
    install(&scene, "15.1.0", b"fake-ripgrep-binary-v1");
    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    std::fs::remove_file(&canonical).unwrap_or_else(|e| panic!("corpus: {e}"));
    std::fs::create_dir_all(&canonical).unwrap_or_else(|e| panic!("corpus: {e}"));

    let source = FileSource::new(scene.root.clone());
    let identity = LAYOUT.identity(&scene.root, "ripgrep");
    let identity_before = std::fs::read(&identity).unwrap_or_default();
    let refusal = refused(scene.run(&source));
    expect_reason(&refusal, RefusalReason::ActivationFailed, "blocked publish");
    assert!(
        canonical.is_dir(),
        "corpus: the blocker stays; the boundary does not delete operator state"
    );
    assert!(
        !LAYOUT
            .last_known_good(&scene.root, "ripgrep", ENTRY)
            .exists(),
        "corpus: no previous state was retained for a publish that never took"
    );
    assert_eq!(
        std::fs::read(&identity).unwrap_or_default(),
        identity_before,
        "corpus: the record is published last, and a failed publish publishes nothing new"
    );
    assert_attempt_clean(&scene);
    scene.cleanup();
}

#[test]
fn an_identity_write_failure_rolls_back() {
    // No previous: the record path is blocked by a directory, the new
    // binary is published, the record cannot stand, and the binary is
    // rolled back rather than left unattributable.
    let scene = Scene::fresh("r21a", true);
    install(&scene, "15.1.0", b"fake-ripgrep-binary-v1");
    let identity = LAYOUT.identity(&scene.root, "ripgrep");
    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    std::fs::remove_file(&canonical).unwrap_or_else(|e| panic!("corpus: {e}"));
    std::fs::remove_file(&identity).unwrap_or_else(|e| panic!("corpus: {e}"));
    std::fs::create_dir_all(&identity).unwrap_or_else(|e| panic!("corpus: {e}"));

    let source = FileSource::new(scene.root.clone());
    let refusal = refused(scene.run(&source));
    expect_reason(
        &refusal,
        RefusalReason::IdentityWriteFailed,
        "blocked record",
    );
    assert!(
        !canonical.exists(),
        "corpus: the unattributable executable is rolled back, not left to run"
    );
    assert!(
        !LAYOUT
            .last_known_good(&scene.root, "ripgrep", ENTRY)
            .exists(),
        "corpus: with no previous state, nothing is restored"
    );
    assert_attempt_clean(&scene);
    scene.cleanup();

    // With a previous: the rollback restores the previous executable so
    // the record and the bytes beside it stay the pair they were.
    let scene = Scene::fresh("r21b", true);
    let previous = b"fake-ripgrep-binary-v1";
    install(&scene, "15.1.0", previous);
    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    let identity = LAYOUT.identity(&scene.root, "ripgrep");
    std::fs::remove_file(&identity).unwrap_or_else(|e| panic!("corpus: {e}"));
    std::fs::create_dir_all(&identity).unwrap_or_else(|e| panic!("corpus: {e}"));

    let source = FileSource::new(scene.root.clone());
    let refusal = refused(scene.run(&source));
    expect_reason(
        &refusal,
        RefusalReason::IdentityWriteFailed,
        "blocked record with previous",
    );
    assert_eq!(
        std::fs::read(&canonical).unwrap_or_default(),
        previous,
        "corpus: the rollback restores the previous executable byte-exact"
    );
    assert!(
        !LAYOUT
            .last_known_good(&scene.root, "ripgrep", ENTRY)
            .exists(),
        "corpus: the retained copy is the canonical again, not a third file"
    );
    assert_attempt_clean(&scene);
    scene.cleanup();
}

#[test]
fn the_attribution_api_exposes_the_commit_and_blob_it_proved() {
    // The corpus exercises attribution through activate; this pins the
    // standalone fact: a committed pin attributes to a 40-hex commit and
    // a 40-hex blob.
    let scene = Scene::fresh("attr", true);
    let digest = Sha256::of(b"archive").hex();
    let pin_bytes = pin_text("15.1.0", &[(TARGET, &digest, ENTRY, SOURCE_REL)]);
    scene.write(PIN_REL, pin_bytes.clone());
    let commit = scene.commit(&[PIN_REL]);
    let attribution = attribute(&scene.root, Path::new(PIN_REL), &pin_bytes)
        .unwrap_or_else(|refusal| panic!("corpus: a committed pin must attribute: {refusal}"));
    assert_eq!(attribution.commit, commit);
    assert_eq!(attribution.commit.len(), 40);
    assert_eq!(attribution.blob.len(), 40);
    scene.cleanup();
}
