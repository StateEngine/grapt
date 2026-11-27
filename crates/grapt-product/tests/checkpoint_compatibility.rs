#![cfg(feature = "checkpoint-fixture")]

use grapt_product::checkpoint_fixture::{run_fixture_corpus, FixtureError};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    law_pack_revision: u64,
    source_pins: BTreeMap<String, String>,
    actions: String,
    actions_sha256: String,
    bounds: Bounds,
    scenarios: Vec<Scenario>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bounds {
    actions: usize,
    checkpoint_lanes: usize,
    fixture_payload_bytes: usize,
    journal_bytes: usize,
    restore_preconditions: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scenario {
    id: String,
    carrier: String,
}

#[derive(Debug)]
struct TestRoot(PathBuf);

impl TestRoot {
    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        // Scoped producers seal the checkout and supply writable TMPDIR.
        // Exclusive creation preserves roots belonging to another fixture.
        let root = std::env::temp_dir().join(format!(
            "grapt-product-checkpoint-{}-{name}",
            std::process::id()
        ));
        let mut directory = std::fs::DirBuilder::new();
        directory.recursive(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directory.mode(0o700);
        }
        directory.create(&root)?;
        Ok(Self(root))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if std::thread::panicking() => {
                eprintln!("{} fixture cleanup failed: {error}", self.0.display());
            }
            Err(error) => panic!("{} fixture cleanup failed: {error}", self.0.display()),
        }
    }
}

#[test]
fn checkpoint_roots_use_private_scratch_without_reusing_existing_data(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("root-ownership")?;
    let scratch = std::env::temp_dir();
    assert_eq!(root.path().parent(), Some(scratch.as_path()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(root.path())?.permissions().mode() & 0o777,
            0o700
        );
    }
    let marker = root.path().join("owned.txt");
    std::fs::write(&marker, b"existing fixture data")?;
    let error = TestRoot::new("root-ownership")
        .err()
        .ok_or("an existing fixture root was reused")?;
    assert_eq!(
        error
            .downcast_ref::<std::io::Error>()
            .map(std::io::Error::kind),
        Some(std::io::ErrorKind::AlreadyExists)
    );
    assert_eq!(std::fs::read(&marker)?, b"existing fixture data");
    let path = root.path().to_path_buf();
    drop(root);
    assert!(matches!(
        std::fs::symlink_metadata(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ));
    Ok(())
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/checkpoint-compatibility")
}

#[test]
fn language_neutral_checkpoint_corpus_executes_through_product_owner(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = fixture_root();
    let manifest: Manifest =
        serde_json::from_slice(&std::fs::read(fixture.join("manifest.json"))?)?;
    assert_eq!(manifest.schema, "grapt.workspace-checkpoint.acceptance.v1");
    assert_eq!(manifest.law_pack_revision, 9);
    assert_eq!(manifest.actions, "actions.jsonl");
    let actions = std::fs::read(fixture.join(&manifest.actions))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &actions);
    let actual = digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(manifest.actions_sha256, actual);
    assert_eq!(manifest.bounds.actions, 32);
    assert_eq!(manifest.bounds.checkpoint_lanes, 32);
    assert_eq!(manifest.bounds.fixture_payload_bytes, 4096);
    assert_eq!(manifest.bounds.journal_bytes, 32_768);
    assert_eq!(manifest.bounds.restore_preconditions, 99);
    assert_eq!(manifest.source_pins.len(), 3);
    assert!(manifest
        .source_pins
        .values()
        .all(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())));
    assert_eq!(manifest.scenarios.len(), 22);
    assert!(manifest
        .scenarios
        .iter()
        .all(|scenario| !scenario.id.is_empty() && !scenario.carrier.is_empty()));

    let root = TestRoot::new("positive")?;
    let receipts = run_fixture_corpus(root.path(), &fixture.join(&manifest.actions))?;
    assert_eq!(receipts.len(), 8);
    assert_eq!(receipts[4].outcome, "ambiguous");
    assert_eq!(receipts[6].outcome, "applied");
    assert_eq!(receipts[7].outcome, "matched");
    Ok(())
}

#[test]
fn structurally_invalid_or_unbounded_actions_cannot_pass_the_corpus_gate(
) -> Result<(), Box<dyn std::error::Error>> {
    let malformed = TestRoot::new("malformed")?;
    let malformed_actions = malformed.path().join("bad.jsonl");
    std::fs::write(&malformed_actions, b"{\"op\":\"capture\",\"seq\":2}\n")?;
    assert!(matches!(
        run_fixture_corpus(malformed.path(), &malformed_actions),
        Err(FixtureError::CorpusInvalid)
    ));

    let over_bound = TestRoot::new("over-bound")?;
    let over_bound_actions = over_bound.path().join("large.jsonl");
    std::fs::write(&over_bound_actions, vec![b'x'; 64 * 1024 + 1])?;
    assert!(matches!(
        run_fixture_corpus(over_bound.path(), &over_bound_actions),
        Err(FixtureError::CorpusOverBound)
    ));
    Ok(())
}

fn continuity_fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/session-continuity")
        .join(name)
}

/// SESSION-CONTINUITY-01 J03: a Git-backed checkpoint pins its exact commit,
/// a restore planned against that baseline refuses once HEAD moves (and never
/// moves HEAD back), an interrupted restore recovers exactly after the product
/// restarts, and an unsupported provider or a corrupt journal record is a
/// truthful failure rather than an absent checkpoint.
#[test]
fn git_backed_checkpoint_recovery_preserves_the_exact_vcs_baseline(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("continuity-git")?;
    let receipts = run_fixture_corpus(
        root.path(),
        &continuity_fixture("checkpoint-git-baseline.jsonl"),
    )?;
    let outcomes = receipts
        .iter()
        .map(|receipt| receipt.outcome.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        outcomes,
        [
            "initialized",
            "written",
            "committed",
            "captured",
            "mutated",
            "planned",
            "committed",
            "refused",
            "matched",
            "matched",
            "planned",
            "ambiguous",
            "reopened",
            "applied",
            "matched",
            "matched",
            "refused",
            "corrupted",
            "corrupt",
        ],
        "the Git-backed continuity corpus changed shape"
    );
    let baseline = receipts[3]
        .baseline
        .as_deref()
        .ok_or("the Git-backed capture recorded no VCS baseline")?;
    let head = baseline
        .strip_prefix("git:")
        .ok_or_else(|| format!("a Git-backed capture pinned a non-Git baseline: {baseline}"))?;
    assert!(
        head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the captured baseline is not an exact commit: {baseline}"
    );
    Ok(())
}

#[test]
fn an_unborn_git_root_refuses_capture_instead_of_pinning_a_missing_baseline(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("continuity-unborn")?;
    let receipts = run_fixture_corpus(
        root.path(),
        &continuity_fixture("checkpoint-unborn-baseline.jsonl"),
    )?;
    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.outcome.as_str())
            .collect::<Vec<_>>(),
        ["initialized", "written", "refused", "matched"],
        "an unborn Git root captured without a baseline"
    );
    Ok(())
}
