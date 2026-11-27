//! Public volatile-store behavior without any filesystem setup or temporary root.
use grapt_artifact::{
    ArtifactKind, ArtifactStore, CaptureBound, CaptureError, CarryDigestBudget, CarryObservation,
    FetchError, FilesystemArtifactStore, ReadBudget, Sensitivity, StatError, Verified,
};
use grapt_types::{ArtifactRef, CaptureDisposition, ClockAuthority, Timestamp};

#[derive(Debug)]
struct FrozenClock;
impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(17)
    }
}

#[test]
fn volatile_clones_share_verified_bounded_artifacts_without_persistence(
) -> Result<(), Box<dyn std::error::Error>> {
    let store = FilesystemArtifactStore::in_memory(b"memory-key".to_vec(), Box::new(FrozenClock));
    struct BrokenSource;
    impl std::io::Read for BrokenSource {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::Other.into())
        }
    }
    let failing = std::io::Read::chain(&b"partial"[..], BrokenSource);
    let full = CaptureBound::new(64 * 1024 * 1024, 16).ok_or("reservation bound")?;
    assert!(matches!(
        store.capture(
            failing,
            full,
            ArtifactKind::Diagnostic,
            Sensitivity::Ordinary
        ),
        Err(CaptureError::Source(_))
    ));
    let shared = store.shared_clone();
    let body = vec![7_u8; 65_539];
    let bound = CaptureBound::new(65_538, 19).ok_or("capture bound")?;
    let captured = store.capture(
        body.as_slice(),
        bound,
        ArtifactKind::Diagnostic,
        Sensitivity::SecretBearing,
    )?;
    let reference = captured.reference();
    assert_eq!(
        captured.disposition(),
        CaptureDisposition::TruncatedAtBoundary { captured: 65_538 }
    );
    let metadata = shared.stat(reference.id())?;
    assert_eq!(metadata.kind(), ArtifactKind::Diagnostic);
    assert_eq!(metadata.captured_at(), Timestamp::from_millis(17));
    let page_budget = ReadBudget::new(65_536).ok_or("page budget")?;
    let first = shared.fetch(reference, 0, page_budget)?;
    assert_eq!(first.bytes(), &body[..65_536]);
    assert_eq!(first.verified(), Verified::Chunk { index: 0 });
    let last = shared.fetch(
        reference,
        first.next_offset().ok_or("next offset")?,
        page_budget,
    )?;
    assert_eq!(last.bytes(), &body[65_536..65_538]);
    assert!(last.next_offset().is_none());
    let whole = shared.fetch(reference, 0, ReadBudget::new(65_538).ok_or("whole budget")?)?;
    assert_eq!(whole.verified(), Verified::Whole);
    let forged = ArtifactRef::new(reference.id().clone(), 1, reference.digest().clone());
    assert_eq!(
        shared.fetch(&forged, 0, page_budget),
        Err(FetchError::Unavailable)
    );
    let mut small = CarryDigestBudget::new(1);
    assert_eq!(
        shared.observe_carried(reference, &mut small),
        CarryObservation::DigestBudgetExceeded
    );
    let mut budget = CarryDigestBudget::new(reference.length());
    assert_eq!(
        shared.observe_carried(reference, &mut budget),
        CarryObservation::Complete
    );
    assert_eq!(budget.remaining(), 0);
    assert_eq!(
        shared.observe_carried(&forged, &mut budget),
        CarryObservation::Corrupt
    );
    let duplicate = shared.capture(
        body.as_slice(),
        bound,
        ArtifactKind::Diagnostic,
        Sensitivity::SecretBearing,
    )?;
    assert_eq!(captured, duplicate);
    let over = CaptureBound::new(u64::MAX, usize::MAX).ok_or("overbound capture")?;
    assert!(matches!(
        shared.capture(
            &b"x"[..],
            over,
            ArtifactKind::Diagnostic,
            Sensitivity::Ordinary
        ),
        Err(CaptureError::Store(_))
    ));
    drop(store);
    assert!(shared.fetch(reference, 0, page_budget).is_ok());
    drop(shared);
    let fresh = FilesystemArtifactStore::in_memory(b"memory-key".to_vec(), Box::new(FrozenClock));
    assert_eq!(fresh.stat(reference.id()), Err(StatError::Unavailable));
    assert_eq!(
        fresh.observe_carried(reference, &mut budget),
        CarryObservation::Absent
    );
    Ok(())
}
