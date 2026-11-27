use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::infra::*;

#[test]
fn a_fresh_install_activates_and_publishes_the_verified_binary() {
    let binary = b"fake-ripgrep-binary-v1";
    let scene = Scene::fresh("fresh", true);
    let receipt = install(&scene, "15.1.0", binary);

    assert_eq!(receipt.outcome, "activated");
    assert_eq!(receipt.tool, "ripgrep");
    assert_eq!(receipt.version, "15.1.0");
    assert_eq!(receipt.target, TARGET);
    assert_eq!(receipt.source, SOURCE_REL);
    assert_eq!(receipt.binary_sha256, Sha256::of(binary).hex());
    assert_eq!(receipt.installed_at_ms, FIXED_MILLIS as u64);
    assert_eq!(receipt.executable, "tools/ripgrep/bin/rg.exe");
    assert_eq!(
        receipt.commit.len(),
        40,
        "corpus: the receipt names the full commit"
    );

    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    assert_eq!(
        std::fs::read(&canonical).unwrap_or_else(|e| panic!("corpus: canonical missing: {e}")),
        binary
    );
    let identity = LAYOUT.identity(&scene.root, "ripgrep");
    assert!(
        identity.is_file(),
        "corpus: the identity record is published last, and it stands"
    );
    assert_attempt_clean(&scene);

    // The receipt wire shape the product seam prints.
    let value: serde_json::Value = serde_json::from_str(&receipt.json()).unwrap_or_default();
    assert_eq!(value["schema"].as_str(), Some("grapt.toolchain.receipt.v1"));
    assert_eq!(value["outcome"].as_str(), Some("activated"));
    assert_eq!(value["commit"].as_str(), Some(receipt.commit.as_str()));
    scene.cleanup();
}

#[test]
fn a_second_identical_call_reuses_without_refetch_or_write() {
    let binary = b"fake-ripgrep-binary-v1";
    let scene = Scene::fresh("reuse", true);
    let first = install(&scene, "15.1.0", binary);
    assert_eq!(first.outcome, "activated");

    let calls = Arc::new(AtomicUsize::new(0));
    let source = CountingSource {
        inner: FileSource::new(scene.root.clone()),
        calls: calls.clone(),
    };
    let second = scene
        .run(&source)
        .unwrap_or_else(|refusal| panic!("corpus: the second call must reuse: {refusal}"));
    assert_eq!(second.outcome, "reused");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "corpus: reuse must not refetch the archive"
    );
    assert_eq!(
        second.installed_at_ms, first.installed_at_ms,
        "corpus: reuse keeps the original record time, never a new stamp"
    );
    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    assert_eq!(
        std::fs::read(&canonical).unwrap_or_default(),
        binary,
        "corpus: reuse must not rewrite the canonical bytes"
    );
    scene.cleanup();
}

#[test]
fn an_upgrade_retains_the_previous_as_last_known_good() {
    let v1 = b"fake-ripgrep-binary-v1";
    let v2 = b"fake-ripgrep-binary-v2";
    let scene = Scene::fresh("upgrade", true);
    install(&scene, "15.1.0", v1);
    let receipt = install(&scene, "15.1.1", v2);
    assert_eq!(receipt.outcome, "activated");

    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    let lkg = LAYOUT.last_known_good(&scene.root, "ripgrep", ENTRY);
    assert_eq!(std::fs::read(&canonical).unwrap_or_default(), v2);
    assert_eq!(
        std::fs::read(&lkg).unwrap_or_default(),
        v1,
        "corpus: the previous executable is retained byte-exact"
    );
    let identity = InstalledIdentity::decode(
        &std::fs::read(LAYOUT.identity(&scene.root, "ripgrep")).unwrap_or_default(),
    )
    .unwrap_or_else(|e| panic!("corpus: identity must decode: {e}"));
    assert_eq!(identity.version.as_str(), "15.1.1");
    assert_eq!(identity.binary_sha256.hex(), Sha256::of(v2).hex());
    scene.cleanup();
}

#[test]
fn a_recut_at_the_same_version_reacquires_and_rewrites_the_identity() {
    // Same version, same target, changed archive bytes: the pin digest
    // changes, and a record written under the old pin must not keep the
    // old binary alive.
    let scene = Scene::fresh("recut", true);
    let old = b"fake-ripgrep-binary-v1";
    let recut = b"fake-ripgrep-binary-recut";
    let first = install(&scene, "15.1.0", old);
    let second = install(&scene, "15.1.0", recut);
    assert_eq!(
        second.outcome, "activated",
        "corpus: a re-cut must re-acquire, never reuse"
    );
    assert_ne!(second.archive_sha256, first.archive_sha256);

    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    let lkg = LAYOUT.last_known_good(&scene.root, "ripgrep", ENTRY);
    assert_eq!(std::fs::read(&canonical).unwrap_or_default(), recut);
    assert_eq!(std::fs::read(&lkg).unwrap_or_default(), old);
    scene.cleanup();
}

#[test]
fn a_revoked_target_refuses_and_leaves_the_activation_untouched() {
    let scene = Scene::fresh("revoked", true);
    install(&scene, "15.1.0", b"fake-ripgrep-binary-v1");
    // The table loses the target entry: a revocation, committed.
    commit_pin(&scene, "15.1.0", &[]);

    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    let identity = LAYOUT.identity(&scene.root, "ripgrep");
    let canonical_before = std::fs::read(&canonical).unwrap_or_default();
    let identity_before = std::fs::read(&identity).unwrap_or_default();

    let source = FileSource::new(scene.root.clone());
    let refusal = refused(scene.run(&source));
    expect_reason(&refusal, RefusalReason::TargetUnsupported, "revoked target");
    assert_eq!(refusal.context.tool.as_deref(), Some("ripgrep"));
    assert_eq!(refusal.context.target.as_deref(), Some(TARGET));
    assert_eq!(
        std::fs::read(&canonical).unwrap_or_default(),
        canonical_before,
        "corpus: revocation must not touch the canonical bytes"
    );
    assert_eq!(
        std::fs::read(&identity).unwrap_or_default(),
        identity_before,
        "corpus: revocation must not touch the record"
    );
    assert_attempt_clean(&scene);
    scene.cleanup();
}

#[test]
fn tampered_canonical_bytes_reacquire_the_verified_binary() {
    let good = b"fake-ripgrep-binary-v1";
    let scene = Scene::fresh("tampered", true);
    install(&scene, "15.1.0", good);
    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    std::fs::write(&canonical, b"attacker-bytes").unwrap_or_else(|e| panic!("corpus: tamper: {e}"));

    let source = FileSource::new(scene.root.clone());
    let receipt = scene
        .run(&source)
        .unwrap_or_else(|refusal| panic!("corpus: a tampered binary must re-acquire: {refusal}"));
    assert_eq!(
        receipt.outcome, "activated",
        "corpus: a mismatched hash is never a vouch"
    );
    assert_eq!(std::fs::read(&canonical).unwrap_or_default(), good);
    let lkg = LAYOUT.last_known_good(&scene.root, "ripgrep", ENTRY);
    assert_eq!(
        std::fs::read(&lkg).unwrap_or_default(),
        b"attacker-bytes",
        "corpus: the tampered bytes are retained as the previous state"
    );
    scene.cleanup();
}

#[test]
fn an_undecodable_identity_never_vouches_and_reacquires() {
    let good = b"fake-ripgrep-binary-v1";
    let scene = Scene::fresh("badidentity", true);
    install(&scene, "15.1.0", good);
    let identity = LAYOUT.identity(&scene.root, "ripgrep");
    std::fs::write(&identity, b"not json at all")
        .unwrap_or_else(|e| panic!("corpus: corrupt identity: {e}"));

    let source = FileSource::new(scene.root.clone());
    let receipt = scene.run(&source).unwrap_or_else(|refusal| {
        panic!("corpus: an undecodable record re-acquires, it never refuses: {refusal}")
    });
    assert_eq!(receipt.outcome, "activated");
    let decoded = InstalledIdentity::decode(&std::fs::read(&identity).unwrap_or_default())
        .unwrap_or_else(|e| panic!("corpus: the record must be rewritten decodable: {e}"));
    assert_eq!(decoded.binary_sha256.hex(), Sha256::of(good).hex());
    scene.cleanup();
}

// The refusal matrix: one test per reason, each asserting the reason and
// the invariants that reason owes.
