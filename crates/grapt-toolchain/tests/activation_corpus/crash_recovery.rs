//! The crash-window and concurrency witnesses.
//!
//! Each test plants the exact state a crash in one named window leaves,
//! or holds the first caller in flight, and asserts what the next attempt
//! does with it: staging residue is untrusted and cleared; a stale lock is
//! refused, naming its holder, never guessed at; a new canonical beside an
//! old record is not vouched and is re-acquired; an orphan last-known-good
//! is cleared by the publish that takes; and concurrent callers serialize
//! on the lock so no partial bytes ever stand at the canonical path.
//! Nothing here reads ambient time: the clock is the corpus fixed clock
//! and the concurrency witness synchronizes on channels only.

use std::path::PathBuf;
use std::sync::mpsc;

use crate::infra::*;

#[test]
fn stale_staging_residue_is_cleared_and_the_attempt_succeeds() {
    // Staging left by a crashed attempt is untrusted: the next attempt
    // clears it however it ends, and a clean reuse succeeds through it.
    let scene = Scene::fresh("crash1", true);
    install(&scene, "15.1.0", b"fake-ripgrep-binary-v1");
    let staging = LAYOUT.staging_dir(&scene.root, "ripgrep");
    std::fs::create_dir_all(&staging).unwrap_or_else(|e| panic!("corpus: {e}"));
    std::fs::write(staging.join("junk.bin"), b"crashed-attempt residue")
        .unwrap_or_else(|e| panic!("corpus: {e}"));

    let source = FileSource::new(scene.root.clone());
    let receipt = scene.run(&source).unwrap_or_else(|refusal| {
        panic!("corpus: reuse must succeed through stale staging: {refusal}")
    });
    assert_eq!(receipt.outcome, "reused");
    assert!(
        !LAYOUT.staging_dir(&scene.root, "ripgrep").exists(),
        "corpus: the untrusted staging root is cleared however the attempt ends"
    );
    scene.cleanup();
}

#[test]
fn stale_staged_binary_residue_is_cleared_and_the_attempt_succeeds() {
    // A staged file left beside the canonical name by a crashed attempt is
    // residue: it is removed before any reuse decision, and an attempt that
    // reuses never renames it.
    let scene = Scene::fresh("crash2", true);
    install(&scene, "15.1.0", b"fake-ripgrep-binary-v1");
    let staged = LAYOUT.staged(&scene.root, "ripgrep", ENTRY);
    std::fs::write(&staged, b"crashed-attempt staged residue")
        .unwrap_or_else(|e| panic!("corpus: {e}"));

    let source = FileSource::new(scene.root.clone());
    let receipt = scene.run(&source).unwrap_or_else(|refusal| {
        panic!("corpus: reuse must succeed through stale staging: {refusal}")
    });
    assert_eq!(receipt.outcome, "reused");
    assert!(
        !LAYOUT.staged(&scene.root, "ripgrep", ENTRY).exists(),
        "corpus: the untrusted staged file is cleared"
    );
    scene.cleanup();
}

#[test]
fn a_planted_stale_lock_refuses_naming_the_holder_then_the_attempt_succeeds() {
    // A lock left by a crashed holder is never guessed at: the refusal
    // names the holder process so an operator can confirm it is gone, the
    // boundary does not remove a lock it did not create, and the attempt
    // stands untouched. Once the operator removes the lock, the state
    // reuses.
    let scene = Scene::fresh("crash3", true);
    install(&scene, "15.1.0", b"fake-ripgrep-binary-v1");
    let lock = LAYOUT.lock(&scene.root, "ripgrep");
    let lock_text = serde_json::json!({
        "schema": "grapt.toolchain.lock.v1",
        "holder_pid": 31337,
        "acquired_at_ms": 1
    })
    .to_string();
    std::fs::write(&lock, lock_text).unwrap_or_else(|e| panic!("corpus: {e}"));

    let source = FileSource::new(scene.root.clone());
    let refusal = refused(scene.run(&source));
    expect_reason(
        &refusal,
        RefusalReason::ActivationInFlight,
        "planted stale lock",
    );
    assert!(
        refusal.detail.contains("31337"),
        "corpus: the refusal must name the holder process: {refusal}"
    );
    assert!(
        lock.exists(),
        "corpus: the boundary never removes a lock it did not create"
    );
    assert_eq!(
        std::fs::read(LAYOUT.canonical(&scene.root, "ripgrep", ENTRY)).unwrap_or_default(),
        b"fake-ripgrep-binary-v1",
        "corpus: the refusal precedes any write"
    );

    std::fs::remove_file(&lock).unwrap_or_else(|e| panic!("corpus: {e}"));
    let receipt = scene.run(&source).unwrap_or_else(|refusal| {
        panic!("corpus: the operator-confirmed state must reuse: {refusal}")
    });
    assert_eq!(receipt.outcome, "reused");
    scene.cleanup();
}

#[test]
fn a_new_canonical_beside_the_old_record_reacquires_and_rewrites() {
    // The publish-to-identity-write crash window: the new canonical bytes
    // stand beside the old record. The old record does not vouch the new
    // bytes, so the attempt re-acquires from the pinned archive and
    // rewrites the record to the pair the law owes.
    let scene = Scene::fresh("crash4", true);
    install(&scene, "15.1.0", b"fake-ripgrep-binary-v1");
    let v2 = b"fake-ripgrep-binary-v2";
    let archive = archive_stream(&[file_entry(ENTRY, v2)]);
    let digest = Sha256::of(&archive).hex();
    scene.write(SOURCE_REL, archive);
    commit_pin(&scene, "15.1.1", &[(TARGET, &digest, ENTRY, SOURCE_REL)]);

    // The crash itself: the publish rename took, the identity write did not.
    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    std::fs::write(&canonical, v2).unwrap_or_else(|e| panic!("corpus: {e}"));

    let source = FileSource::new(scene.root.clone());
    let receipt = scene.run(&source).unwrap_or_else(|refusal| {
        panic!("corpus: the mismatched state must re-acquire: {refusal}")
    });
    assert_eq!(receipt.outcome, "activated");
    assert_eq!(std::fs::read(&canonical).unwrap_or_default(), v2);
    let identity = InstalledIdentity::decode(
        &std::fs::read(LAYOUT.identity(&scene.root, "ripgrep")).unwrap_or_default(),
    )
    .unwrap_or_else(|e| panic!("corpus: the rewritten record must decode: {e}"));
    assert_eq!(identity.version.as_str(), "15.1.1");
    assert_eq!(identity.binary_sha256.hex(), Sha256::of(v2).hex());
    scene.cleanup();
}

#[test]
fn a_publish_window_orphan_last_known_good_is_cleared_and_the_attempt_succeeds() {
    // A last-known-good with nothing canonical beside it is orphan residue
    // from a crashed publish. The next publish clears it unconditionally:
    // the retained copy belongs to the publish that takes, never to the one
    // that crashed.
    let scene = Scene::fresh("crash5", true);
    let v1 = b"fake-ripgrep-binary-v1";
    install(&scene, "15.1.0", v1);
    let canonical = LAYOUT.canonical(&scene.root, "ripgrep", ENTRY);
    let last_good = LAYOUT.last_known_good(&scene.root, "ripgrep", ENTRY);
    std::fs::remove_file(&canonical).unwrap_or_else(|e| panic!("corpus: {e}"));
    std::fs::write(&last_good, v1).unwrap_or_else(|e| panic!("corpus: {e}"));

    let source = FileSource::new(scene.root.clone());
    let receipt = scene.run(&source).unwrap_or_else(|refusal| {
        panic!("corpus: the attempt must re-acquire without a canonical: {refusal}")
    });
    assert_eq!(receipt.outcome, "activated");
    assert_eq!(std::fs::read(&canonical).unwrap_or_default(), v1);
    assert!(
        !last_good.exists(),
        "corpus: the orphan last-known-good is cleared by the publish that takes"
    );
    scene.cleanup();
}

/// A source that announces it has started fetching, then blocks until
/// released: holds the lock across the fetch for the concurrency witness.
#[derive(Debug)]
struct GatedSource {
    inner: FileSource,
    started: mpsc::SyncSender<()>,
    released: mpsc::Receiver<()>,
}

impl ArchiveSource for GatedSource {
    fn fetch(&self, source: &SourceUrl) -> Result<Vec<u8>, Refusal> {
        self.started
            .send(())
            .unwrap_or_else(|e| panic!("corpus: {e}"));
        self.released
            .recv()
            .unwrap_or_else(|e| panic!("corpus: {e}"));
        self.inner.fetch(source)
    }
}

#[test]
fn concurrent_callers_serialize_without_exposing_partial_bytes() {
    // One attempt holds the lock across the fetch. A second caller is
    // refused, names the holder, and writes nothing: the canonical path
    // stays empty while the first attempt is in flight, and it publishes
    // the whole verified bytes, never partial.
    let v1 = b"fake-ripgrep-binary-v1";
    let archive = archive_stream(&[file_entry(ENTRY, v1)]);
    let digest = Sha256::of(&archive).hex();
    let scene = Scene::fresh("crash6", true);
    scene.write(
        PIN_REL,
        pin_text("15.1.0", &[(TARGET, &digest, ENTRY, SOURCE_REL)]),
    );
    scene.write(SOURCE_REL, archive);
    scene.commit(&[PIN_REL, SOURCE_REL]);

    let (started_tx, started_rx) = mpsc::sync_channel(0);
    let (released_tx, released_rx) = mpsc::sync_channel(0);
    let gated = GatedSource {
        inner: FileSource::new(scene.root.clone()),
        started: started_tx,
        released: released_rx,
    };
    let root = scene.root.clone();
    let first = std::thread::spawn(move || {
        let clock = FixedClock;
        let request = ActivationRequest {
            root,
            pin: PathBuf::from(PIN_REL),
            target: must_target(),
            platform: PLATFORM.to_owned(),
            arch: ARCH.to_owned(),
            clock: &clock,
            source: &gated,
        };
        activate(&request)
    });

    started_rx
        .recv()
        .unwrap_or_else(|e| panic!("corpus: the first caller never started: {e}"));
    let plain = FileSource::new(scene.root.clone());
    let refusal = refused(scene.run(&plain));
    expect_reason(&refusal, RefusalReason::ActivationInFlight, "second caller");
    assert!(
        refusal.detail.contains(&std::process::id().to_string()),
        "corpus: the refusal must name the holder process: {refusal}"
    );
    assert!(
        !LAYOUT.canonical(&scene.root, "ripgrep", ENTRY).exists(),
        "corpus: no partial bytes at the canonical path while an attempt is in flight"
    );

    released_tx
        .send(())
        .unwrap_or_else(|e| panic!("corpus: release the first caller: {e}"));
    let receipt = first
        .join()
        .unwrap_or_else(|e| panic!("corpus: the first caller panicked: {e:?}"))
        .unwrap_or_else(|refusal| panic!("corpus: the first caller must publish: {refusal}"));
    assert_eq!(receipt.outcome, "activated");
    assert_eq!(
        std::fs::read(LAYOUT.canonical(&scene.root, "ripgrep", ENTRY)).unwrap_or_default(),
        v1
    );
    let identity = InstalledIdentity::decode(
        &std::fs::read(LAYOUT.identity(&scene.root, "ripgrep")).unwrap_or_default(),
    )
    .unwrap_or_else(|e| panic!("corpus: the record must decode: {e}"));
    assert_eq!(identity.binary_sha256.hex(), Sha256::of(v1).hex());
    scene.cleanup();
}
