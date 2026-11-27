//! The provenance/aggregate grid for the Rust-native session envelope.
//!
//! Two variants each carry exactly one provenance class, so the control the
//! admission names — "a Rust-native kind cannot ride a Manifest-only
//! constructor path, and the reverse" — is a constructor refusal with a typed
//! error, witnessable in BOTH directions rather than argued.

use grapt_event::{AggregateId, EnvelopeKind, EventEnvelopeV2, EventKind, KindProvenance};
use grapt_types::{EventId, EventSeq, SessionId, Timestamp, WriterEpoch};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn global_form_kinds_round_trip_only_on_their_typed_form_aggregate() -> TestResult {
    for kind in [
        EventKind::FormCreatedV2,
        EventKind::FormSuspendedV2,
        EventKind::FormRepliedV2,
        EventKind::FormCancelledV2,
    ] {
        let form = AggregateId::Form("frm_global_grid".parse()?);
        let envelope = build(EnvelopeKind::Form(kind), form.clone())?;
        assert_eq!(
            grapt_event::decode_canonical(&envelope.canonical_bytes())?.canonical_bytes(),
            envelope.canonical_bytes()
        );
        assert!(build(EnvelopeKind::Form(kind), session()?).is_err());
        assert!(build(EnvelopeKind::RustSession(kind), session()?).is_err());
        assert!(build(EnvelopeKind::Legacy(kind), form.clone()).is_err());
        assert!(build(EnvelopeKind::Form(EventKind::FormCreatedV1), form).is_err());
    }
    Ok(())
}

fn session() -> Result<AggregateId, Box<dyn std::error::Error>> {
    Ok(AggregateId::Session(
        "ses_033601c26ffe".parse::<SessionId>()?,
    ))
}

fn build(
    kind: EnvelopeKind,
    aggregate: AggregateId,
) -> Result<EventEnvelopeV2, grapt_event::EnvelopeError> {
    // `unwrap_or_else` rather than `expect`: the workspace denies
    // `clippy::expect_used`, and a fixture id that failed to parse should be
    // a refusal this harness reports, not a panic dressed as a message.
    let id = match "evt_033601c26ffe".parse::<EventId>() {
        Ok(id) => id,
        Err(_) => unreachable!("the fixture event id is a literal and parses"),
    };
    let epoch = match WriterEpoch::new(1) {
        Some(epoch) => epoch,
        None => unreachable!("one is a positive epoch"),
    };
    EventEnvelopeV2::new(
        id,
        aggregate,
        EventSeq::new(1),
        epoch,
        kind,
        Timestamp::from_millis(1_786_147_200_000),
        b"{}".to_vec(),
    )
}

#[test]
fn a_rust_native_kind_rides_only_the_rust_session_variant() -> TestResult {
    let native = EventKind::SessionCompactionEndedV2;
    assert_eq!(native.provenance(), KindProvenance::RustNative);

    // Accepted on its own variant.
    let accepted = build(EnvelopeKind::RustSession(native), session()?)?;
    assert_eq!(accepted.kind().kind_str(), "session.compaction.ended.2");

    // Refused on Legacy, and refused as a PROVENANCE mismatch rather than as a
    // historical kind — the two refusals are different facts about why the
    // write is illegal, and collapsing them would hide which law fired.
    assert_eq!(
        build(EnvelopeKind::Legacy(native), session()?).err(),
        Some(grapt_event::EnvelopeError::ProvenanceMismatch {
            kind: native,
            expected: KindProvenance::Manifest,
            actual: KindProvenance::RustNative,
        })
    );
    Ok(())
}

#[test]
fn a_manifest_kind_rides_only_the_legacy_variant() -> TestResult {
    // The mirror direction. Without it, `RustSession` would be a second door
    // into every oracle kind and the label would stop meaning anything.
    let manifest = EventKind::SessionCompactionEndedV1;
    assert_eq!(manifest.provenance(), KindProvenance::Manifest);

    assert!(build(EnvelopeKind::Legacy(manifest), session()?).is_ok());
    assert_eq!(
        build(EnvelopeKind::RustSession(manifest), session()?).err(),
        Some(grapt_event::EnvelopeError::ProvenanceMismatch {
            kind: manifest,
            expected: KindProvenance::RustNative,
            actual: KindProvenance::Manifest,
        })
    );
    Ok(())
}

#[test]
fn a_historical_kind_rides_neither_variant() -> TestResult {
    let historical = EventKind::ALL
        .iter()
        .copied()
        .find(|kind| {
            matches!(
                kind.provenance(),
                KindProvenance::HistoricalLogOnly | KindProvenance::HistoricalDeleted
            )
        })
        .ok_or("the vocabulary must still contain a historical kind")?;

    // Legacy keeps its own unchanged refusal: no new legacy event may be
    // written, whatever variant is offered.
    assert_eq!(
        build(EnvelopeKind::Legacy(historical), session()?).err(),
        Some(grapt_event::EnvelopeError::HistoricalKind { kind: historical })
    );
    // And the new variant is not an escape hatch for it either.
    assert!(matches!(
        build(EnvelopeKind::RustSession(historical), session()?),
        Err(grapt_event::EnvelopeError::ProvenanceMismatch { .. })
    ));
    Ok(())
}

#[test]
fn the_rust_native_variant_must_ride_a_session_aggregate() -> TestResult {
    // A session fact is fenced session-scoped, so riding another family's
    // aggregate would fence it against the wrong elected writer.
    let form = AggregateId::Form("frm_grid".parse()?);
    assert!(matches!(
        build(
            EnvelopeKind::RustSession(EventKind::SessionCompactionEndedV2),
            form
        ),
        Err(grapt_event::EnvelopeError::KindAggregateMismatch {
            kind_family: "rust-native-session",
            ..
        })
    ));
    Ok(())
}
