//! Witness for the per-block publication latch.
//!
//! Benchmark ae5ce3a8: a refused live Step opening refused every text delta,
//! and each refusal invalidated every subscriber, so a fast answer ended the
//! ACP event stream more than its 32 rebinds allow.

use super::*;

#[path = "provisional_snapshot_tests.rs"]
mod provisional_snapshot;

fn sink(root: &Path, hub: Arc<DogfoodEventHub>) -> Result<LiveSink, String> {
    let inbox =
        Inbox::new(grapt_store::EventStore::open_in_memory().map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())?;
    let session: SessionId = "ses_latch_witness"
        .parse()
        .map_err(|_| "session id refused".to_owned())?;
    Ok(LiveSink {
        artifacts: Arc::new(open_artifacts(root).map_err(|error| error.to_string())?),
        tools: BTreeMap::new(),
        inbox: Arc::new(Mutex::new(inbox)),
        hub,
        ids: Arc::new(OsRandomIds::seed().map_err(|error| error.to_string())?),
        clock: Arc::new(SystemClock),
        session,
        execution: ExecutionId::new("exe_latch_witness").ok_or("execution refused")?,
        publication: Arc::new(PublicationLedger::new()),
        blocks: Vec::new(),
        pending: std::collections::VecDeque::new(),
        refused: Vec::new(),
        attempt: None,
        text_ordinal: 0,
        reasoning_ordinal: 0,
        attempt_ordinals: (0, 0),
        step: StepNumber::new(1).ok_or("step refused")?,
        agent: WireId::parse("agt_latch_witness".to_owned()).ok_or("agent refused")?,
        model: super::super::decode(serde_json::json!({"providerID":"openai","id":"gpt-live"}))?,
        message: None,
    })
}

/// Whether this subscriber was invalidated: an invalidated host queue
/// refuses at once, while a live one still holds its connected frame.
fn invalidated(subscription: &grapt_api::u3a::DogfoodEventSubscription) -> bool {
    subscription.next_frame().is_err()
}

#[test]
fn a_refused_block_invalidates_subscribers_once_not_per_delta() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("grapt-latch-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let hub = super::super::event_hub()?;
    let mut sink = sink(&root, Arc::clone(&hub))?;
    let first = hub.subscribe_host().map_err(|error| error.to_string())?;
    // No opened block: the delta is refused, as every delta was when the
    // Step opening disagreed.
    sink.latched_delta(0, ContentKind::Text, "token ");
    assert!(invalidated(&first), "the first refusal must invalidate");
    let rebound = hub.subscribe_host().map_err(|error| error.to_string())?;
    for _ in 0..40 {
        sink.latched_delta(0, ContentKind::Text, "token ");
    }
    assert!(
        !invalidated(&rebound),
        "later deltas of a refused block ended the rebound stream again"
    );
    // The next Step starts unlatched. A fresh subscriber, because a read of
    // an empty live queue would wait rather than fail.
    let next = hub.subscribe_host().map_err(|error| error.to_string())?;
    sink.step_began(StepNumber::new(2).ok_or("step refused")?);
    sink.latched_delta(0, ContentKind::Text, "token ");
    assert!(
        invalidated(&next),
        "a new Step's refusal must still invalidate"
    );
    let _cleanup = std::fs::remove_dir_all(&root);
    Ok(())
}

/// Review of fe5a83e4: an empty block closes with no capture, so a
/// refused empty block is never settled by one. Sequential refusals of a
/// kind replace each other; the latch stays bounded by the content kinds.
#[test]
fn sequential_refused_blocks_never_grow_the_latch_past_one_per_kind() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("grapt-latch-bound-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let hub = super::super::event_hub()?;
    let mut sink = sink(&root, hub)?;
    for block in 0..100 {
        sink.latched_delta(block, ContentKind::Text, "token ");
        sink.latched_delta(1_000 + block, ContentKind::Reasoning, "thinking ");
    }
    assert_eq!(sink.refused.len(), 2, "{:?}", sink.refused);
    let _cleanup = std::fs::remove_dir_all(&root);
    Ok(())
}

/// A refusal for size alone ends a subscriber's
/// stream only after it has drained what was already queued; any other
/// refusal still closes it at once. The subscriber here has read nothing,
/// as a client busy writing to its own client has not, so its queue holds
/// the frame published before the refusal (its connected frame).
#[test]
fn a_bound_refusal_drains_queued_frames_before_ending_the_stream() -> Result<(), String> {
    let root = std::env::temp_dir().join(format!("grapt-drain-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let hub = super::super::event_hub()?;
    let sink = sink(&root, Arc::clone(&hub))?;

    let unread = hub.subscribe_host().map_err(|error| error.to_string())?;
    sink.publication_refusal(ExecutionRefusal::Run(format!(
        "{}: SSE event exceeds its byte bound",
        super::live_sink::BOUND_REFUSAL
    )));
    let queued = unread
        .next_frame()
        .map_err(|error| format!("queued frame dropped: {error}"))?;
    assert!(
        queued.is_some(),
        "the frame queued before the refusal never arrived"
    );
    assert!(
        matches!(unread.next_frame(), Ok(None)),
        "the drained stream did not end cleanly"
    );

    let other = hub.subscribe_host().map_err(|error| error.to_string())?;
    sink.publication_refusal(ExecutionRefusal::Run("projection disagreed".into()));
    assert!(
        invalidated(&other),
        "an integrity refusal must close the stream at once"
    );
    let _cleanup = std::fs::remove_dir_all(&root);
    Ok(())
}

/// The JSON of each frame a cancelled subscriber drains, or why its stream
/// failed: an invalidated stream refuses at once.
fn drained(subscription: &grapt_api::u3a::DogfoodEventSubscription) -> Result<Vec<String>, String> {
    subscription.cancel();
    let mut frames = Vec::new();
    while let Some(frame) = subscription
        .next_frame()
        .map_err(|error| format!("the stream was invalidated: {error}"))?
    {
        let frame = String::from_utf8_lossy(&frame).into_owned();
        frames.push(
            frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .unwrap_or_default()
                .to_owned(),
        );
    }
    Ok(frames)
}

/// Elision ruling: a block at the public bound, whose ended
/// frame cannot carry its whole text, ends with the elided frame (its length
/// and digest) and the stream lives on; it used to end every stream. Plain
/// text at the bound, and control characters whose JSON escapes overflow far
/// below it; text and reasoning alike.
#[test]
fn an_at_bound_block_ends_with_an_elided_frame_and_the_stream_lives() -> Result<(), String> {
    use grapt_event::session_message::{
        ContentAppendedFact, ContentBlockKind, ContentBlockOpenedFact,
    };
    use grapt_types::{
        ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition, ContentOrdinal,
        NormalizedContentItem,
    };
    let root = std::env::temp_dir().join(format!("grapt-elision-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let cases = [
        (
            ContentKind::Text,
            ContentBlockKind::Text,
            "session.text.ended",
        ),
        (
            ContentKind::Reasoning,
            ContentBlockKind::Reasoning,
            "session.reasoning.ended",
        ),
    ];
    let texts = [
        "a".repeat(super::super::PUBLIC_CONTENT_BYTES),
        "\u{1}".repeat(50_000),
    ];
    for ((kind, block_kind, ended_type), text) in cases
        .iter()
        .flat_map(|case| texts.iter().map(move |text| (case.clone(), text.clone())))
    {
        let hub = super::super::event_hub()?;
        let mut sink = sink(&root, Arc::clone(&hub))?;
        let subscriber = hub.subscribe_host().map_err(|error| error.to_string())?;
        let message: MessageId = "msg_elision_witness"
            .parse()
            .map_err(|_| "message refused")?;
        let attempt = PhysicalAttemptId::new("pat_elision_witness").ok_or("attempt refused")?;
        let opening = ContentBlockOpenedFact::new(attempt, message.clone(), 7, block_kind);
        let position = grapt_session::CommittedPosition {
            event: "evt_elision_open".parse().map_err(|_| "event refused")?,
            seq: 4,
            created_ms: 123,
        };
        sink.blocks.push(LiveBlock {
            block: 7,
            kind: kind.clone(),
            opening,
            position: position.clone(),
            text: text.clone(),
            public: Some((position.seq, 0)),
        });
        let captured = ContentAppendedFact::new(
            message,
            NormalizedContentItem::new(
                ContentOrdinal::new(3),
                kind,
                ArtifactRef::new(
                    ArtifactId::new("art_elision_witness").ok_or("artifact refused")?,
                    text.len() as u64,
                    ArtifactDigest::new("sha256:elision").ok_or("digest refused")?,
                ),
            ),
            CaptureDisposition::Complete,
        )
        .map_err(|error| error.to_string())?;
        let ended_position = grapt_session::CommittedPosition {
            event: "evt_elision_content".parse().map_err(|_| "event refused")?,
            seq: 5,
            created_ms: 456,
        };
        sink.publish_capture(&ended_position, &captured)
            .map_err(|error| format!("the at-bound capture was refused: {error}"))?;
        let frames = drained(&subscriber)?;
        let ended = frames
            .iter()
            .find(|frame| frame.contains(ended_type))
            .ok_or_else(|| {
                format!(
                    "no ended frame reached the subscriber: {} frames",
                    frames.len()
                )
            })?;
        let value: serde_json::Value =
            serde_json::from_str(ended).map_err(|error| error.to_string())?;
        let digest: String = ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            value["data"]["text"],
            "",
            "{}",
            &ended[..ended.len().min(200)]
        );
        assert_eq!(value["data"]["textBytes"], text.len() as u64);
        assert_eq!(value["data"]["textSha256"], digest.as_str());
    }
    let _cleanup = std::fs::remove_dir_all(&root);
    Ok(())
}

/// The coalescing ceiling and the live event bound, pinned together: a delta
/// of the ceiling's size, all control characters (each escapes to six JSON
/// bytes, the worst case), publishes as ONE frame within the SSE event bound.
/// Raising the ceiling past what one frame can carry fails here, not on a
/// user's long answer under load (the at-bound ACP refusal).
///
/// One edge stays, deliberately: a SINGLE provider event within about 100
/// bytes of the provider's own 256 KiB frame bound could re-encode a few bytes
/// over this bound (the two envelopes differ). Real providers stream
/// token-sized deltas, so it is unreachable from them, and an abnormal
/// endpoint degrades honestly to a reconcile.
#[test]
fn a_ceiling_sized_delta_of_worst_case_text_fits_one_live_event() -> Result<(), String> {
    use grapt_event::session_message::{ContentBlockKind, ContentBlockOpenedFact};
    let root = std::env::temp_dir().join(format!("grapt-coalesce-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let hub = super::super::event_hub()?;
    let mut sink = sink(&root, Arc::clone(&hub))?;
    let subscriber = hub.subscribe_host().map_err(|error| error.to_string())?;
    let message: MessageId = "msg_coalesce_witness"
        .parse()
        .map_err(|_| "message refused")?;
    let attempt = PhysicalAttemptId::new("pat_coalesce_witness").ok_or("attempt refused")?;
    sink.message = Some(message.clone());
    sink.blocks.push(LiveBlock {
        block: 7,
        kind: ContentKind::Text,
        opening: ContentBlockOpenedFact::new(attempt, message, 7, ContentBlockKind::Text),
        position: grapt_session::CommittedPosition {
            event: "evt_coalesce_open".parse().map_err(|_| "event refused")?,
            seq: 4,
            created_ms: 123,
        },
        text: String::new(),
        public: None,
    });
    let worst = "\u{1}".repeat(grapt_provider::AttemptFragment::MAX_COALESCED_DELTA_BYTES);
    sink.publish_delta(7, &worst)
        .map_err(|error| format!("a ceiling-sized delta was refused: {error}"))?;
    let frames = drained(&subscriber)?;
    let delta = frames
        .iter()
        .find(|frame| frame.contains("session.text.delta"))
        .ok_or_else(|| {
            format!(
                "no delta frame reached the subscriber: {} frames",
                frames.len()
            )
        })?;
    assert!(
        delta.len() <= grapt_api::u3a::MAX_SSE_EVENT_BYTES,
        "the delta frame is {} bytes, over the {}-byte event bound",
        delta.len(),
        grapt_api::u3a::MAX_SSE_EVENT_BYTES
    );
    let _cleanup = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn a_hosted_tool_publishes_its_committed_v2_opening_and_terminal(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_runtime::AttemptOwner as _;
    struct Profile(PathBuf);
    impl Drop for Profile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let path = std::env::temp_dir().join(format!(
        "grapt-hosted-publication-{}",
        OsRandomIds::seed()?.instance()
    ));
    std::fs::create_dir(&path)?;
    let profile = Profile(path);
    let hub = super::super::event_hub()?;
    let sink = sink(&profile.0, hub.clone())?;
    let publication = sink.publication.clone();
    let session = sink.session.clone();
    let subscriber = hub.subscribe_host()?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        sink.inbox.clone(),
        open_artifacts(&profile.0)?,
        sink.clock.clone(),
        sink.ids.clone(),
        sink,
        session.clone(),
        StepNumber::new(1).ok_or("step")?,
        None,
    )?;
    owner.commit(DurableFact::AttemptStarted {
        attempt: PhysicalAttemptId::new("pat_hosted_publication").ok_or("attempt")?,
    })?;
    let call = ToolCallId::new("ws_live_publication").ok_or("call")?;
    owner.commit(DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::ToolCalled {
            call: call.clone(),
            tool: "web_search".into(),
            input: r#"{"query":"fixture"}"#.into(),
            execution: grapt_types::ToolExecution::ProviderHosted,
            state: grapt_provider::AdapterState::none(),
        }),
    })?;
    assert_eq!(
        publication.take(&session),
        None,
        "hosted opening refused publication"
    );
    let output = owner.capture_hosted_output("provider-hosted result")?;
    owner.commit(DurableFact::ToolTerminal {
        call,
        outcome: ToolTerminalOutcome::Completed {
            output: Some(output),
        },
    })?;
    assert_eq!(
        publication.take(&session),
        None,
        "hosted terminal refused publication"
    );
    owner.commit(DurableFact::StepEnded {
        finish: grapt_provider::FinishReason::Stop,
    })?;
    drop(owner);
    // Closing this test subscriber drains already queued frames and makes
    // missing publication fail without waiting for a future producer.
    subscriber.cancel();
    let mut frames = String::new();
    for _ in 0..=64 {
        let Some(frame) = subscriber.next_frame()? else {
            break;
        };
        frames.push_str(std::str::from_utf8(&frame)?);
    }
    assert!(frames.contains("session.tool.input.started"));
    assert!(frames.contains("session.tool.called"));
    assert!(frames.contains("session.tool.success"));
    assert!(frames.contains("provider-hosted result"));
    drop(subscriber);
    Ok(())
}

/// SC-REL-02b: a retried attempt's open blocks are discarded uncommitted. A
/// subscriber that saw them is told to reload, and the retry takes the public
/// ordinals they held, so live and hydrated ordinals cannot collide.
#[test]
fn a_retried_attempt_rewinds_its_public_ordinals_and_invalidates_once_seen() -> Result<(), String> {
    use grapt_event::session_message::{ContentBlockKind, ContentBlockOpenedFact};
    let root = std::env::temp_dir().join(format!("grapt-retry-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let message: MessageId = "msg_retry_witness".parse().map_err(|_| "message refused")?;
    let attempt = PhysicalAttemptId::new("pat_retry_witness").ok_or("attempt refused")?;
    for published in [true, false] {
        let hub = super::super::event_hub()?;
        let mut sink = sink(&root, Arc::clone(&hub))?;
        let subscriber = hub.subscribe_host().map_err(|error| error.to_string())?;
        // One finished text before this attempt: its ordinal stays taken.
        sink.text_ordinal = 1;
        sink.attempt_ordinals = (1, 0);
        sink.reasoning_ordinal = u64::from(published);
        let position = grapt_session::CommittedPosition {
            event: "evt_retry_open".parse().map_err(|_| "event refused")?,
            seq: 9,
            created_ms: 123,
        };
        sink.blocks.push(LiveBlock {
            block: 0,
            kind: ContentKind::Reasoning,
            opening: ContentBlockOpenedFact::new(
                attempt.clone(),
                message.clone(),
                0,
                ContentBlockKind::Reasoning,
            ),
            position: position.clone(),
            text: if published {
                "weighing".into()
            } else {
                String::new()
            },
            public: published.then_some((position.seq, 0)),
        });
        sink.open_blocks_discarded();
        assert!(sink.blocks.is_empty());
        assert_eq!((sink.text_ordinal, sink.reasoning_ordinal), (1, 0));
        assert_eq!(
            invalidated(&subscriber),
            published,
            "only a block a subscriber saw makes it reload"
        );
    }
    let _cleanup = std::fs::remove_dir_all(&root);
    Ok(())
}

/// SC-REL-02c: finished reasoning waits for the owner's deferred capture
/// instead of being refused at its close; the capture ends it, and a
/// discarded attempt drops it with a reload for subscribers that saw it.
#[test]
fn held_reasoning_ends_on_its_deferred_capture_or_drops_with_its_attempt() -> Result<(), String> {
    use grapt_event::session_message::{
        ContentAppendedFact, ContentBlockKind, ContentBlockOpenedFact,
    };
    use grapt_types::{
        ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition, ContentOrdinal,
        NormalizedContentItem,
    };
    let root = std::env::temp_dir().join(format!("grapt-held-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    let message: MessageId = "msg_held_witness".parse().map_err(|_| "message refused")?;
    let attempt = PhysicalAttemptId::new("pat_held_witness").ok_or("attempt refused")?;
    for discard in [false, true] {
        let hub = super::super::event_hub()?;
        let mut sink = sink(&root, Arc::clone(&hub))?;
        let subscriber = hub.subscribe_host().map_err(|error| error.to_string())?;
        sink.message = Some(message.clone());
        sink.attempt = Some(attempt.clone());
        let opening = ContentBlockOpenedFact::new(
            attempt.clone(),
            message.clone(),
            0,
            ContentBlockKind::Reasoning,
        );
        let position = grapt_session::CommittedPosition {
            event: "evt_held_open".parse().map_err(|_| "event refused")?,
            seq: 3,
            created_ms: 100,
        };
        sink.open_block(&position, &opening)
            .map_err(|error| format!("open refused: {error}"))?;
        sink.publish_delta(0, "weighing")
            .map_err(|error| format!("delta refused: {error}"))?;
        sink.offer_fragment(&AttemptFragment::ReasoningClosed {
            block: grapt_provider::BlockOrdinal::new(0),
            state: grapt_provider::AdapterState::none(),
        })
        .map_err(|error| format!("a held close was refused: {error}"))?;
        assert!(sink.blocks.is_empty());
        assert_eq!(
            sink.pending.len(),
            1,
            "the closed block waits for its capture"
        );
        if discard {
            sink.open_blocks_discarded();
            assert!(sink.pending.is_empty());
            assert!(
                invalidated(&subscriber),
                "a seen held block makes subscribers reload"
            );
            continue;
        }
        let captured = ContentAppendedFact::new(
            message.clone(),
            NormalizedContentItem::new(
                ContentOrdinal::FIRST,
                ContentKind::Reasoning,
                ArtifactRef::new(
                    ArtifactId::new("art_held_witness").ok_or("artifact refused")?,
                    8,
                    ArtifactDigest::new("sha256:held").ok_or("digest refused")?,
                ),
            ),
            CaptureDisposition::Complete,
        )
        .map_err(|error| error.to_string())?;
        let ended_position = grapt_session::CommittedPosition {
            event: "evt_held_content".parse().map_err(|_| "event refused")?,
            seq: 7,
            created_ms: 200,
        };
        sink.content_captured(&ended_position, &captured);
        assert!(sink.pending.is_empty());
        let frames = drained(&subscriber)?;
        assert!(
            frames
                .iter()
                .any(|frame| frame.contains("session.reasoning.ended")),
            "the deferred capture ends the held block"
        );
    }
    let _cleanup = std::fs::remove_dir_all(&root);
    Ok(())
}
