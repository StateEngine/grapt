//! Snapshot membership follows the real live producer's held-block lifetimes.
use super::*;
use grapt_api::{ProvisionalKind, ProvisionalText};
use grapt_event::session_message::{ContentAppendedFact, ContentBlockKind, ContentBlockOpenedFact};
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition, ContentOrdinal,
    NormalizedContentItem,
};

#[path = "../../../../../test-support/tmp_guard.rs"]
mod tmp_guard;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open(sink: &mut LiveSink, block: u32, reasoning: bool) -> TestResult {
    let kind = if reasoning {
        ContentBlockKind::Reasoning
    } else {
        ContentBlockKind::Text
    };
    let fact = ContentBlockOpenedFact::new(
        sink.attempt.clone().ok_or("attempt")?,
        sink.message.clone().ok_or("message")?,
        block,
        kind,
    );
    let position = grapt_session::CommittedPosition {
        event: format!("evt_snapshot_open_{block}").parse()?,
        seq: u64::from(block) + 3,
        created_ms: 10,
    };
    sink.open_block(&position, &fact)?;
    sink.publish_delta(block, &format!("block-{block} Ω"))?;
    Ok(())
}

fn held(sink: &mut LiveSink, block: u32) -> TestResult {
    open(sink, block, true)?;
    sink.offer_fragment(&AttemptFragment::ReasoningClosed {
        block: grapt_provider::BlockOrdinal::new(block),
        state: grapt_provider::AdapterState::none(),
    })?;
    Ok(())
}

fn initialize(root: &Path, hub: Arc<DogfoodEventHub>) -> TestResult<LiveSink> {
    let mut sink = sink(root, hub)?;
    sink.message = Some("msg_snapshot_blocks".parse()?);
    sink.attempt = Some(PhysicalAttemptId::new("pat_snapshot_blocks").ok_or("attempt")?);
    Ok(sink)
}

fn has(blocks: &[ProvisionalText], kind: ProvisionalKind, ordinal: u64, text: &str) -> bool {
    blocks.iter().any(|held| {
        held.block.kind == kind
            && held.block.ordinal == ordinal
            && held.text.as_deref() == Some(text)
    })
}

#[test]
fn every_held_reasoning_block_remains_until_its_own_durable_end_or_discard() -> TestResult {
    let root = tmp_guard::Dir::new("grapt-snapshot-held")?;
    let hub = super::super::super::event_hub()?;
    let mut sink = initialize(&root, Arc::clone(&hub))?;
    for block in 0..16 {
        held(&mut sink, block)?;
    }
    open(&mut sink, 16, true)?;
    open(&mut sink, 17, false)?;
    let before = hub.provisional(&sink.session)?;
    assert_eq!(before.len(), 18);
    for ordinal in 0..17 {
        assert!(has(
            &before,
            ProvisionalKind::Reasoning,
            ordinal,
            &format!("block-{ordinal} Ω")
        ));
    }
    assert!(has(&before, ProvisionalKind::Text, 0, "block-17 Ω"));
    let capture = ContentAppendedFact::new(
        sink.message.clone().ok_or("message")?,
        NormalizedContentItem::new(
            ContentOrdinal::FIRST,
            ContentKind::Reasoning,
            ArtifactRef::new(
                ArtifactId::new("art_snapshot_block").ok_or("artifact")?,
                10,
                ArtifactDigest::new("sha256:snapshot").ok_or("digest")?,
            ),
        ),
        CaptureDisposition::Complete,
    )?;
    sink.content_captured(
        &grapt_session::CommittedPosition {
            event: "evt_snapshot_capture".parse()?,
            seq: 50,
            created_ms: 20,
        },
        &capture,
    );
    let after = hub.provisional(&sink.session)?;
    assert_eq!(after.len(), 17);
    assert!(!after
        .iter()
        .any(|held| held.block.kind == ProvisionalKind::Reasoning && held.block.ordinal == 0));
    assert!(has(&after, ProvisionalKind::Reasoning, 1, "block-1 Ω"));
    assert!(has(&after, ProvisionalKind::Text, 0, "block-17 Ω"));
    sink.open_blocks_discarded();
    assert!(hub.provisional(&sink.session)?.is_empty());
    Ok(())
}

#[test]
fn refusing_one_open_block_preserves_held_reasoning_and_the_healthy_other_kind() -> TestResult {
    let root = tmp_guard::Dir::new("grapt-snapshot-refusal")?;
    let hub = super::super::super::event_hub()?;
    let mut sink = initialize(&root, Arc::clone(&hub))?;
    held(&mut sink, 0)?;
    held(&mut sink, 1)?;
    open(&mut sink, 2, true)?;
    open(&mut sink, 3, false)?;
    sink.refuse_block(
        2,
        ContentKind::Reasoning,
        ExecutionRefusal::Run("synthetic publication refusal".into()),
    );
    let remaining = hub.provisional(&sink.session)?;
    assert_eq!(remaining.len(), 3);
    assert!(has(&remaining, ProvisionalKind::Reasoning, 0, "block-0 Ω"));
    assert!(has(&remaining, ProvisionalKind::Reasoning, 1, "block-1 Ω"));
    assert!(has(&remaining, ProvisionalKind::Text, 0, "block-3 Ω"));
    assert!(!remaining
        .iter()
        .any(|held| held.block.kind == ProvisionalKind::Reasoning && held.block.ordinal == 2));
    sink.open_blocks_discarded();
    assert!(hub.provisional(&sink.session)?.is_empty());
    Ok(())
}
