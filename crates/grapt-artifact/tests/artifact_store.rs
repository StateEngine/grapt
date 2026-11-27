//! Acceptance witnesses for the bounded artifact capture/store seam.
//!
//! Each witness is built so that ONLY the mechanism it names separates its two
//! sides. Where two laws could both fire on the same input, the input is shaped
//! so that exactly one can — otherwise a single witness would cover two laws and
//! attribute neither.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use grapt_artifact::{
    ArtifactKind, ArtifactStore, CaptureBound, CarryDigestBudget, CarryObservation, FetchError,
    FilesystemArtifactStore, ReadBudget, Sensitivity, StatError, Verified,
};
use grapt_types::{ArtifactRef, CaptureDisposition, ClockAuthority, Timestamp};

/// The injected clock. Production time is never read in this crate, so a
/// capture receipt is reproducible.
#[derive(Debug)]
struct FrozenClock(i64);

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

static ROOTS: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let ordinal = ROOTS.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "grapt-artifact-{}-{name}-{ordinal}",
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

fn store(scratch: &Scratch, millis: i64) -> FilesystemArtifactStore {
    match FilesystemArtifactStore::open(
        &scratch.0,
        b"store-key".to_vec(),
        Box::new(FrozenClock(millis)),
    ) {
        Ok(store) => store,
        Err(error) => panic!("opening a scratch store must succeed: {error}"),
    }
}

fn bound(max: u64, buf: usize) -> CaptureBound {
    match CaptureBound::new(max, buf) {
        Some(bound) => bound,
        None => panic!("the test bound must be valid"),
    }
}

fn budget(bytes: u64) -> ReadBudget {
    match ReadBudget::new(bytes) {
        Some(budget) => budget,
        None => panic!("the test budget must be valid"),
    }
}

fn capture(
    store: &FilesystemArtifactStore,
    body: &[u8],
    max: u64,
) -> grapt_artifact::CapturedArtifact {
    match store.capture(
        body,
        bound(max, 64),
        ArtifactKind::CapturedOutput,
        Sensitivity::Ordinary,
    ) {
        Ok(captured) => captured,
        Err(error) => panic!("capture must succeed: {error}"),
    }
}

include!("artifact_store/identity_and_integrity.rs");
include!("artifact_store/bounded_reads_and_kinds.rs");
include!("artifact_store/sensitivity_and_metadata.rs");
include!("artifact_store/carry_admission.rs");
