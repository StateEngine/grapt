//! Read-depth carry checks: structure without a byte digest, pooled in order.
//!
//! A read route checks records, objects and lengths only; every byte it serves
//! is digested by the fetch that returns it. Writer admission keeps the digest.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use grapt_artifact::{
    ArtifactKind, ArtifactStore, CaptureBound, CarryDigestBudget, CarryObservation, CarryWorkers,
    FilesystemArtifactStore, Sensitivity,
};
use grapt_types::{ArtifactRef, ClockAuthority, Timestamp};

#[derive(Debug)]
struct FrozenClock;

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1)
    }
}

static ROOTS: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let ordinal = ROOTS.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "grapt-carry-structural-{}-{name}-{ordinal}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn store(scratch: &Scratch) -> FilesystemArtifactStore {
    FilesystemArtifactStore::open(&scratch.0, b"store-key".to_vec(), Box::new(FrozenClock))
        .unwrap_or_else(|error| panic!("opening a scratch store must succeed: {error}"))
}

fn capture(store: &FilesystemArtifactStore, body: &[u8]) -> ArtifactRef {
    let bound = CaptureBound::new(4096, 64).unwrap_or_else(|| panic!("valid test bound"));
    store
        .capture(
            body,
            bound,
            ArtifactKind::CapturedOutput,
            Sensitivity::Ordinary,
        )
        .unwrap_or_else(|error| panic!("capture must succeed: {error}"))
        .reference()
        .clone()
}

fn find_object(root: &Path, id: &str) -> PathBuf {
    root.join("objects")
        .join(id.get(..2).unwrap_or("__"))
        .join(id)
}

fn test_budget() -> CarryDigestBudget {
    CarryDigestBudget::new(16 * 1024 * 1024)
}

#[test]
fn the_structural_check_leaves_a_same_length_flip_to_the_digest() {
    let scratch = Scratch::new("carry-structural-flip");
    let store = store(&scratch);
    let reference = capture(&store, b"bytes a reader is served only by fetch");
    let object = find_object(&scratch.0, reference.id().as_str());
    let mut bytes = std::fs::read(&object).unwrap_or_else(|error| panic!("read: {error}"));
    if let Some(byte) = bytes.first_mut() {
        *byte ^= 0x01;
    }
    std::fs::write(&object, &bytes).unwrap_or_else(|error| panic!("rewrite: {error}"));

    assert_eq!(
        store.observe_carried_structure(&reference),
        CarryObservation::Complete,
        "records, objects and lengths still stand",
    );
    assert_eq!(
        store.observe_carried(&reference, &mut test_budget()),
        CarryObservation::Corrupt,
        "only the digest sees a same-length flip",
    );
}

#[test]
fn the_pooled_structural_check_reports_the_first_failure_in_reference_order() {
    let scratch = Scratch::new("carry-structural-pool");
    let store = store(&scratch);
    let references: Vec<ArtifactRef> = (0..64)
        .map(|index| capture(&store, format!("carried artifact {index}").as_bytes()))
        .collect();
    let complete = CarryWorkers::new(16);
    assert_eq!(
        store.first_structural_carry_failure(&references, &complete),
        None
    );
    // A later reference fails too, so a worker that finishes it first must not
    // decide the answer.
    let truncated = find_object(&scratch.0, references[41].id().as_str());
    std::fs::write(&truncated, b"short").unwrap_or_else(|error| panic!("truncate: {error}"));
    let removed = find_object(&scratch.0, references[9].id().as_str());
    std::fs::remove_file(&removed).unwrap_or_else(|error| panic!("remove: {error}"));
    let expected = Some((&references[9], CarryObservation::Absent));
    for helpers in [0, 1, 16] {
        let workers = CarryWorkers::new(helpers);
        for _ in 0..8 {
            assert_eq!(
                store.first_structural_carry_failure(&references, &workers),
                expected,
                "helpers={helpers}",
            );
        }
    }
    // Concurrent reads share one small budget: each still gets the same answer.
    let shared = CarryWorkers::new(2);
    std::thread::scope(|scope| {
        let readers: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| store.first_structural_carry_failure(&references, &shared)))
            .collect();
        for reader in readers {
            match reader.join() {
                Ok(answer) => assert_eq!(answer, expected),
                Err(_) => panic!("a concurrent reader panicked"),
            }
        }
    });
}
