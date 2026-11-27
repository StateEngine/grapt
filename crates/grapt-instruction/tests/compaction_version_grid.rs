//! The right-version grid for the compaction boundary, end to end.
//!
//! One semantic boundary now exists at two payload versions. The grid proves
//! all four cells rather than the happy one: the Rust-native v2 fact is
//! accepted and advances the instruction epoch, a v2-shaped payload offered at
//! v1 is refused by the UNTOUCHED frozen decoder, the frozen v1 fact is still
//! accepted unchanged, and v3 is still refused. The middle two are what stop
//! the new version from becoming a way to relax the old one.

use grapt_event::session_compaction;
use grapt_instruction::{InstructionService, RefreshInput, SourceObservation};
use grapt_store::{AggregateId, AppendInput, EventStore};
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CompactionDescriptor, CompactionDescriptorParts,
    CompactionId, CompactionSummary, CompactionTrigger, ContextEpoch, ContextWindowId, EventId,
    EventVersion, InstructionKey, NextRequestRef, RetainedHistory, RetentionPolicyVersion,
    SessionId, StepNumber,
};
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn artifact(id: &str) -> Option<ArtifactRef> {
    Some(ArtifactRef::new(
        ArtifactId::new(id)?,
        7,
        ArtifactDigest::new("digest")?,
    ))
}

fn descriptor(session: &SessionId) -> Option<CompactionDescriptor> {
    CompactionDescriptor::new(CompactionDescriptorParts {
        id: CompactionId::new("cmp-1")?,
        source_session: session.clone(),
        source_epoch: ContextEpoch::new(1),
        target_window: ContextWindowId::new("cwn-1")?,
        target_epoch: ContextEpoch::new(2),
        trigger: CompactionTrigger::ContextOverflow,
        retention_policy: RetentionPolicyVersion::new("ret-1")?,
        summary: CompactionSummary::ClientReadable {
            artifact: artifact("sum-1")?,
        },
        retained: RetainedHistory::References {
            artifacts: vec![artifact("hist-1")?],
        },
        baseline: artifact("base-1")?,
        next_request: NextRequestRef::new(StepNumber::new(1)?, artifact("req-1")?),
    })
    .ok()
}

fn append(
    store: &mut EventStore,
    id: &str,
    session: &SessionId,
    kind: &str,
    version: u32,
    data: &Value,
) -> Result<(), Box<dyn std::error::Error>> {
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let mut projector = grapt_instruction::InstructionProjector;
    let (_appended, notices) = store.append(
        AppendInput {
            id: &id.parse::<EventId>()?,
            aggregate,
            writer,
            kind_name: kind,
            version: EventVersion::new(version).ok_or("version")?,
            created_ms: 1_786_147_200_000,
            data,
        },
        &mut [&mut projector as &mut dyn grapt_store::Projector],
    )?;
    notices.discard();
    Ok(())
}

/// A session with one committed instruction state, so a boundary has an epoch
/// to advance.
fn seeded() -> Result<(EventStore, SessionId), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    InstructionService::ensure_tables(&mut store)?;
    let session: SessionId = "ses_033601c26ffe".parse()?;
    let key = InstructionKey::new("AGENTS.md").ok_or("key")?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    InstructionService::refresh(
        &mut store,
        RefreshInput {
            event: "evt_0336000000seed".parse()?,
            session: session.clone(),
            created_ms: 1_786_147_200_000,
            required: vec![key.clone()],
            observations: vec![SourceObservation::Available {
                key,
                value: grapt_instruction::InstructionValue::Text("seed".into()),
            }],
        },
    )?;
    Ok((store, session))
}

fn epoch(store: &EventStore, session: &SessionId) -> Result<u64, Box<dyn std::error::Error>> {
    Ok(InstructionService::history(store, session)?
        .ok_or("history")?
        .epoch
        .value())
}

// Admission and epoch advance are TWO laws, and they are deliberately two
// tests. Held together in one function they are indistinguishable evidence: a
// missing v2 arm and a v2 arm that forgets the epoch both fail the same test,
// and the first failure short-circuits before the second law is ever reached.
// The mutation harness proved exactly that — the two mutants produced
// byte-identical (witness, detail) evidence and its uniqueness guard refused
// the run. One law witnessed twice is not two laws witnessed.

#[test]
fn the_rust_native_v2_fact_is_accepted() -> TestResult {
    let (mut store, session) = seeded()?;
    let payload = session_compaction::encode(&descriptor(&session).ok_or("descriptor")?);

    // Asserted rather than propagated with `?`: a propagated error carries no
    // panic message, so this law's failure would be reported as an anonymous
    // test error indistinguishable from any neighbouring one.
    let accepted = append(
        &mut store,
        "evt_03360000000v2a",
        &session,
        "session.compaction.ended",
        2,
        &payload,
    );
    assert!(
        accepted.is_ok(),
        "the EXPLICIT v2 arm must accept the Rust-native fact; without it the \
         version catch-all refuses it as an unsupported version: {accepted:?}"
    );
    Ok(())
}

#[test]
fn the_rust_native_v2_fact_advances_the_instruction_epoch() -> TestResult {
    let (mut store, session) = seeded()?;
    let before = epoch(&store, &session)?;
    let payload = session_compaction::encode(&descriptor(&session).ok_or("descriptor")?);

    append(
        &mut store,
        "evt_03360000000v2e",
        &session,
        "session.compaction.ended",
        2,
        &payload,
    )?;

    let after = epoch(&store, &session)?;
    assert!(
        after > before,
        "the v2 boundary must advance the instruction epoch exactly as v1 does: \
         a fact that decoded but left the epoch behind keeps pre-compaction \
         instruction state live"
    );
    Ok(())
}

#[test]
fn a_v2_shaped_payload_offered_at_v1_is_refused_by_the_frozen_decoder() -> TestResult {
    // The frozen v1 strict_object is UNTOUCHED by this lane, and this is the
    // witness that it still refuses the rich shape. Without it, "v1 stays
    // frozen" would be an assertion about a diff rather than about behavior.
    let (mut store, session) = seeded()?;
    let payload = session_compaction::encode(&descriptor(&session).ok_or("descriptor")?);
    assert!(append(
        &mut store,
        "evt_03360000000v1r",
        &session,
        "session.compaction.ended",
        1,
        &payload,
    )
    .is_err());
    Ok(())
}

#[test]
fn a_v1_shaped_payload_offered_at_v2_is_refused_by_the_new_codec() -> TestResult {
    // The mirror. The two contracts must refuse each other in BOTH directions,
    // or one version becomes a relaxed door into the other.
    let (mut store, session) = seeded()?;
    assert!(append(
        &mut store,
        "evt_03360000000v2r",
        &session,
        "session.compaction.ended",
        2,
        &json!({
            "sessionID": session.as_str(),
            "reason": "auto",
            "text": "compacted",
            "recent": "recent"
        }),
    )
    .is_err());
    Ok(())
}

#[test]
fn the_frozen_v1_fact_is_still_accepted_unchanged() -> TestResult {
    let (mut store, session) = seeded()?;
    let before = epoch(&store, &session)?;
    append(
        &mut store,
        "evt_03360000000v1a",
        &session,
        "session.compaction.ended",
        1,
        &json!({
            "sessionID": session.as_str(),
            "reason": "auto",
            "text": "compacted",
            "recent": "recent"
        }),
    )?;
    assert!(epoch(&store, &session)? > before);
    Ok(())
}

#[test]
fn a_third_version_and_an_unknown_name_are_still_refused() -> TestResult {
    let (mut store, session) = seeded()?;
    let payload = session_compaction::encode(&descriptor(&session).ok_or("descriptor")?);

    // v3 is refused by the closed vocabulary — the existing catch-all keeps
    // working for free, which is precisely what admitting v2 must not change.
    assert!(append(
        &mut store,
        "evt_03360000000v3r",
        &session,
        "session.compaction.ended",
        3,
        &payload,
    )
    .is_err());

    assert!(append(
        &mut store,
        "evt_0336000000unk",
        &session,
        "session.compaction.descriptor.recorded",
        1,
        &payload,
    )
    .is_err());
    Ok(())
}

#[test]
fn a_v2_fact_naming_another_session_is_refused() -> TestResult {
    // Same law v1 already enforces: a fact must name the aggregate it rides,
    // or a corrupt foreign event advances this session's epoch.
    let (mut store, session) = seeded()?;
    let other: SessionId = "ses_033601c26fff".parse()?;
    let payload = session_compaction::encode(&descriptor(&other).ok_or("descriptor")?);
    assert!(append(
        &mut store,
        "evt_0336000000fgn",
        &session,
        "session.compaction.ended",
        2,
        &payload,
    )
    .is_err());
    Ok(())
}
