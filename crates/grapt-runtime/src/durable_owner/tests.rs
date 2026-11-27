//! Witnesses for the durable owner.
//!
//! Each test drives real `DurableFact`s through the owner against a real
//! in-memory `EventStore` and a real filesystem artifact store, then folds the
//! resulting log with `grapt_session::MessageHistory` and asserts what a
//! reader sees. That round trip is the point: an owner test that asserted only
//! on the owner's own fields would prove the owner agrees with itself.

use super::*;
use grapt_artifact::FilesystemArtifactStore;
use grapt_provider::{AdapterState, BlockOrdinal, FinishReason};
use grapt_session::{HistoryRow, MessageHistory, StepTerminal};
use grapt_store::EventStore;
use grapt_types::usage::{UsageField, UsageProvenance};
use grapt_types::{
    CaptureDisposition, InterruptionSource, ProviderCompletionDisposition, SessionId,
    StepTerminalReason, Timestamp, TokenCount, ToolTerminalOutcome,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// A frozen clock. Every durable `created` stamp in these witnesses is this
/// value, so a changed timestamp is a changed FACT rather than test noise.
#[derive(Debug)]
struct FrozenClock;

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_784_090_756_114)
    }
}

/// Deterministic identities, counted from one.
///
/// Not random: a witness that cannot name the identity it expects cannot
/// assert that a retry reused it.
#[derive(Debug, Default)]
struct CountingIds {
    next: AtomicU64,
}

impl IdAuthority for CountingIds {
    fn mint(&self) -> grapt_types::IdToken {
        let value = self.next.fetch_add(1, Ordering::SeqCst);
        match grapt_types::IdToken::new(format!("{value:012x}")) {
            Some(token) => token,
            None => panic!("a hex counter renders non-empty"),
        }
    }
}

/// A scratch directory removed when the test ends.
#[derive(Debug)]
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "grapt-df0-durable-owner-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }

    fn artifacts(&self) -> FilesystemArtifactStore {
        match FilesystemArtifactStore::open(
            &self.0,
            b"df0-owner-key".to_vec(),
            Box::new(FrozenClock),
        ) {
            Ok(store) => store,
            Err(error) => panic!("scratch artifact store must open: {error}"),
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Every fact the owner accepted, in commit order — so a witness can assert
/// that publication happened after commit and exactly once.
#[derive(Debug, Default)]
struct RecordingSink {
    offered: Vec<DurableFact>,
}

impl CommittedFactSink for RecordingSink {
    fn offer(&mut self, fact: &DurableFact) {
        self.offered.push(fact.clone());
    }
}

#[derive(Debug)]
struct CoordinateSink {
    store: Arc<Mutex<EventStore>>,
    notices: Vec<(CommittedPosition, serde_json::Value)>,
}

impl CoordinateSink {
    fn record(&mut self, position: &CommittedPosition, data: serde_json::Value) {
        let store = self
            .store
            .try_lock()
            .unwrap_or_else(|_| panic!("publication still holds the commit transaction"));
        let row: (i64, i64, String) = store
            .connection()
            .query_row(
                "SELECT seq, created, data FROM event WHERE id = ?1",
                [position.event.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap_or_else(|error| panic!("notice preceded its committed row: {error}"));
        assert_eq!(u64::try_from(row.0).ok(), Some(position.seq));
        assert_eq!(row.1, position.created_ms);
        let persisted: serde_json::Value = serde_json::from_str(&row.2)
            .unwrap_or_else(|error| panic!("committed payload is unreadable: {error}"));
        assert_eq!(persisted, data);
        self.notices.push((position.clone(), data));
    }
}

impl CommittedFactSink for CoordinateSink {
    fn offer(&mut self, _fact: &DurableFact) {}
    fn content_opened(&mut self, position: &CommittedPosition, fact: &ContentBlockOpenedFact) {
        self.record(
            position,
            grapt_event::session_message::encode_block_opened(fact),
        );
    }
    fn content_captured(&mut self, position: &CommittedPosition, fact: &ContentAppendedFact) {
        self.record(
            position,
            grapt_event::session_message::encode_appended(fact),
        );
    }
}

#[test]
fn reused_normalized_block_numbers_publish_exact_distinct_commits(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new("reused-block");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let sink = CoordinateSink {
        store: Arc::clone(&store),
        notices: Vec::new(),
    };
    let mut owner = DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        sink,
        session(),
        step(1),
        None,
    )?;
    owner.commit(DurableFact::AttemptStarted {
        attempt: attempt("pat_reused"),
    })?;
    for text in ["first", "other"] {
        drive(
            &mut owner,
            vec![text_opened(0), text_delta(0, text), text_closed(0)],
        );
    }
    let notices = &owner.sink.notices;
    assert_eq!(notices.len(), 4);
    assert_eq!(notices[0].1["block"], 0);
    assert_eq!(notices[2].1["block"], 0);
    assert_ne!(notices[0].0.event, notices[2].0.event);
    assert_ne!(notices[1].0.event, notices[3].0.event);
    assert_eq!(notices[1].1["item"]["ordinal"], 0);
    assert_eq!(notices[3].1["item"]["ordinal"], 1);
    assert_eq!(notices[1].1["item"]["payload"]["length"], 5);
    assert_eq!(notices[3].1["item"]["payload"]["length"], 5);
    assert_ne!(
        notices[1].1["item"]["payload"]["digest"],
        notices[3].1["item"]["payload"]["digest"]
    );
    Ok(())
}

mod fixtures;
mod nested;
use fixtures::*;

/// THE SUCCESS CUT. One Step: reasoning then text then a clean terminal. The
/// log folds to one message with two content items in emission order and one
/// terminal.
#[test]
fn one_step_writes_one_message_with_ordered_content_and_one_terminal() {
    let scratch = Scratch::new("success");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };

    drive(
        &mut owner,
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            reasoning_opened(0),
            reasoning_delta(0, "thinking"),
            reasoning_closed(0),
            text_opened(1),
            text_delta(1, "hello "),
            text_delta(1, "world"),
            text_closed(1),
            DurableFact::StepEnded {
                finish: FinishReason::Stop,
            },
        ],
    );

    let history = history(&store);
    let messages = match history.messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the folded history must render: {error}"),
    };
    assert_eq!(messages.len(), 1, "one Step mints exactly one message");
    let only = &messages[0];
    assert_eq!(only.step, step(1));
    assert_eq!(only.attempt, attempt("pat_1"));
    let content = match &only.content {
        Some(content) => content,
        None => panic!("the message has content"),
    };
    // TWO items, not four: two deltas coalesced into one text block, and the
    // reasoning block kept its position ahead of it.
    assert_eq!(content.len(), 2);
    assert_eq!(content.items()[0].item.ordinal(), ContentOrdinal::FIRST);
    assert_eq!(content.items()[0].item.kind(), &ContentKind::Reasoning);
    assert_eq!(content.items()[1].item.ordinal(), ContentOrdinal::new(1));
    assert_eq!(content.items()[1].item.kind(), &ContentKind::Text);
    // The text item's artifact holds BOTH deltas: 11 bytes, not 6.
    assert_eq!(content.items()[1].item.payload().length(), 11);
    assert_eq!(
        only.terminal,
        Some(StepTerminal::Ended {
            completion: ProviderCompletionDisposition::AssertedComplete
        })
    );
    assert!(owner.is_settled());
    assert!(owner.message().is_some());
}

/// REPLAY CAUSES ZERO PROVIDER CALLS AND ZERO TERMINAL RESURRECTION.
///
/// The owner is dropped and the durable log is folded again. The second read
/// touches no provider — there is none to touch — and yields exactly the same
/// history, with exactly one terminal.
#[test]
fn replaying_the_durable_log_yields_the_same_history_and_one_terminal() {
    let scratch = Scratch::new("replay");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    {
        let mut owner = match DurableAttemptOwner::elect(
            Arc::clone(&store),
            scratch.artifacts(),
            &clock,
            &ids,
            RecordingSink::default(),
            session(),
            step(1),
            None,
        ) {
            Ok(owner) => owner,
            Err(error) => panic!("the owner must elect: {error}"),
        };
        drive(
            &mut owner,
            vec![
                DurableFact::AttemptStarted {
                    attempt: attempt("pat_1"),
                },
                text_opened(0),
                text_delta(0, "settled"),
                text_closed(0),
                DurableFact::StepEnded {
                    finish: FinishReason::Stop,
                },
            ],
        );
    }
    let first = match history(&store).messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the first read must render: {error}"),
    };
    let second = match history(&store).messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the second read must render: {error}"),
    };
    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
    assert!(matches!(
        first[0].terminal,
        Some(StepTerminal::Ended { .. })
    ));
    // And the durable log itself holds exactly ONE Step terminal row.
    let terminals = rows(&store)
        .into_iter()
        .filter(|(_, kind, _)| kind == "session.step.ended.2" || kind == "session.step.failed.2")
        .count();
    assert_eq!(terminals, 1);
}

/// THE INTERRUPT FAILURE CUT. A Step interrupted mid-stream keeps the content
/// that closed, writes its tool terminal, and settles ONCE as a failure —
/// never as a success.
#[test]
fn an_interrupted_step_settles_once_as_a_failure_and_keeps_closed_content() {
    interrupted_content(true);
}

#[test]
fn interruption_before_block_close_preserves_the_observed_bytes_on_replay() {
    interrupted_content(false);
}

fn interrupted_content(closed: bool) {
    let scratch = Scratch::new(if closed {
        "interrupt"
    } else {
        "interrupt-open"
    });
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    let call = match ToolCallId::new("call_1") {
        Some(call) => call,
        None => panic!("call_1 is a non-empty call identity"),
    };
    let mut facts = vec![
        DurableFact::AttemptStarted {
            attempt: attempt("pat_1"),
        },
        text_opened(0),
        text_delta(0, "partial"),
        DurableFact::ToolTerminal {
            call: call.clone(),
            outcome: ToolTerminalOutcome::Cancelled {
                by: InterruptionSource::User,
            },
        },
        DurableFact::StepFailed {
            failure: crate::owner::StepFailure::new(
                StepTerminalReason::Interrupted {
                    by: InterruptionSource::User,
                },
                grapt_provider::BoundedDetail::from("interrupted"),
            ),
        },
    ];
    if closed {
        facts.insert(3, text_closed(0));
    }
    drive(&mut owner, facts);

    let history = history(&store);
    let messages = match history.messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the folded history must render: {error}"),
    };
    // The partial text survives whether interruption preceded block close or followed it.
    let content = match &messages[0].content {
        Some(content) => content,
        None => panic!("the observed block must produce an item"),
    };
    assert_eq!(content.len(), 1);
    assert_eq!(content.items()[0].item.payload().length(), 7);
    // The terminal is a FAILURE, and a cancelled tool is never a success.
    assert!(matches!(
        messages[0].terminal,
        Some(StepTerminal::Failed {
            reason: StepTerminalReason::Interrupted { .. }
        })
    ));
    let tool_terminals = rows(&store)
        .into_iter()
        .filter(|(_, kind, _)| kind == "session.tool.terminal.1")
        .count();
    assert_eq!(tool_terminals, 1);
}

/// THE RESURRECTION MUTANT'S TARGET. A second Step terminal is refused BY THE
/// OWNER, before it can reach the log. Without this guard the fold would be
/// the only thing standing between a resurrected Step and a reader.
#[test]
fn a_second_step_terminal_is_refused_by_the_owner() {
    let scratch = Scratch::new("second-terminal");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    drive(
        &mut owner,
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            DurableFact::StepEnded {
                finish: FinishReason::Stop,
            },
        ],
    );
    let refusal = owner.commit(DurableFact::StepEnded {
        finish: FinishReason::Stop,
    });
    match refusal {
        Err(error) => assert!(
            error
                .reason
                .contains("already wrote its one durable terminal"),
            "the refusal must name the law it enforces, got: {}",
            error.reason
        ),
        Ok(()) => panic!("a second Step terminal must be refused"),
    }
    // And nothing reached the log: still exactly one terminal row.
    let terminals = rows(&store)
        .into_iter()
        .filter(|(_, kind, _)| kind == "session.step.ended.2")
        .count();
    assert_eq!(terminals, 1);
}

/// A finish that normalizes to a FAILURE cannot be written as a Step success.
/// `ContentFilter` is a content-policy refusal, and publishing it as a clean
/// completion would be a synthetic success terminal.
#[test]
fn a_failure_shaped_finish_cannot_be_written_as_a_step_success() {
    let scratch = Scratch::new("finish-failure");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    let refusal = owner.commit(DurableFact::StepEnded {
        finish: FinishReason::ContentFilter,
    });
    match refusal {
        Err(error) => assert!(
            error.reason.contains("normalizes to a failure"),
            "got: {}",
            error.reason
        ),
        Ok(()) => panic!("a content-policy finish is not a Step success"),
    }
    assert!(!owner.is_settled());
    assert!(rows(&store).is_empty());
}

/// USAGE: an absent field stays absent through the durable round trip, and the
/// terminal observation is a LATER fact than the stream one rather than an
/// overwrite.
#[test]
fn usage_observations_reach_the_log_with_their_phase_and_absent_fields() {
    let scratch = Scratch::new("usage");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    drive(
        &mut owner,
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            DurableFact::Fragment {
                fragment: Box::new(AttemptFragment::Usage {
                    usage: usage(None, Some(3)),
                }),
            },
            DurableFact::Fragment {
                fragment: Box::new(AttemptFragment::ProviderTerminal {
                    finish: FinishReason::Stop,
                    usage: Some(complete_usage()),
                    state: state(),
                }),
            },
        ],
    );
    let history = history(&store);
    let observed = history.usage_for(&attempt("pat_1"));
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].phase(), UsageObservationPhase::DuringStream);
    assert!(
        matches!(observed[0].input_tokens(), Some(field) if field.is_absent()),
        "an unreported input count is ABSENT, never a zero"
    );
    assert_eq!(observed[0].output_tokens().value(), Some(3));
    assert_eq!(observed[1].phase(), UsageObservationPhase::Terminal);
    assert_eq!(
        observed[1].input_total_tokens().and_then(UsageField::value),
        Some(14)
    );
    assert_eq!(
        observed[1].input_tokens().and_then(UsageField::value),
        Some(11)
    );
    assert_eq!(
        observed[1].reasoning_tokens().value(),
        Some(3),
        "durable reported reasoning usage was lost"
    );
    assert_eq!(observed[1].cache_read_input_tokens().value(), Some(2));
    assert_eq!(observed[1].cache_write_input_tokens().value(), Some(1));
    // Every value came off the provider's wire; none is presented as estimated.
    assert_eq!(
        observed[1].input_tokens().and_then(UsageField::provenance),
        Some(&UsageProvenance::ProviderReported)
    );
}

/// CRASH CUT: the process disappears after a usage observation commits but
/// before any provider or Step terminal. Restart folds the one versioned
/// observation exactly once; replaying the read cannot resurrect another row.
#[test]
fn usage_survives_restart_before_terminal_without_duplication() {
    let scratch = Scratch::new("usage-crash-cut");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    {
        let mut owner = match DurableAttemptOwner::elect(
            Arc::clone(&store),
            scratch.artifacts(),
            &clock,
            &ids,
            RecordingSink::default(),
            session(),
            step(1),
            None,
        ) {
            Ok(owner) => owner,
            Err(error) => panic!("the owner must elect: {error}"),
        };
        drive(
            &mut owner,
            vec![
                DurableFact::AttemptStarted {
                    attempt: attempt("pat_usage_crash"),
                },
                DurableFact::Fragment {
                    fragment: Box::new(AttemptFragment::Usage {
                        usage: usage(None, Some(3)),
                    }),
                },
            ],
        );
        assert!(
            !owner.is_settled(),
            "the crash cut accidentally wrote a terminal"
        );
    }

    let first = history(&store)
        .usage_for(&attempt("pat_usage_crash"))
        .to_vec();
    let restarted = history(&store)
        .usage_for(&attempt("pat_usage_crash"))
        .to_vec();
    assert_eq!(first, restarted);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].phase(), UsageObservationPhase::DuringStream);
    assert_eq!(first[0].output_tokens().value(), Some(3));

    let durable = rows(&store);
    assert_eq!(
        durable
            .iter()
            .filter(|(_, kind, _)| kind == "session.usage.observed.2")
            .count(),
        1,
        "restart/replay duplicated the committed usage observation"
    );
    assert!(
        durable.iter().all(|(_, kind, _)| {
            kind != "session.step.ended.2" && kind != "session.step.failed.2"
        }),
        "the crash cut invented a terminal"
    );
}

/// An empty block mints no ordinal and no artifact. Capturing zero bytes would
/// put an item in the transcript for content the model never emitted.
#[test]
fn an_empty_block_mints_no_content_item() {
    let scratch = Scratch::new("empty-block");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    drive(
        &mut owner,
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            text_opened(0),
            text_closed(0),
        ],
    );
    // The attempt has a replayable identity, but no fabricated content item.
    assert!(owner.message().is_some());
    let recorded = rows(&store);
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].1, "session.assistant-message.opened.1");
    assert_eq!(recorded[1].1, "session.content-block.opened.1");
}

#[test]
fn attempt_opening_is_replayable_before_content_and_retries_reuse_it() {
    let scratch = Scratch::new("opening-retry");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    drive(
        &mut owner,
        vec![DurableFact::AttemptStarted {
            attempt: attempt("pat_1"),
        }],
    );
    let first = rows(&store);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].1, "session.assistant-message.opened.1");
    let message = owner.message().cloned();
    assert!(message.is_some());
    let projected = match history(&store).messages() {
        Ok(messages) => messages,
        Err(error) => panic!("an opening without content must replay: {error}"),
    };
    assert_eq!(projected.len(), 1);
    drive(
        &mut owner,
        vec![DurableFact::AttemptStarted {
            attempt: attempt("pat_2"),
        }],
    );
    assert_eq!(owner.message(), message.as_ref());
    assert_eq!(rows(&store), first, "retry must not append another opening");
    drive(
        &mut owner,
        vec![
            text_opened(0),
            text_delta(0, "retry output"),
            text_closed(0),
        ],
    );
    let recorded = rows(&store);
    assert_eq!(recorded.len(), 3);
    assert_eq!(recorded[0], first[0]);
}

#[test]
fn selected_model_commits_with_opening_before_any_content() -> Result<(), Box<dyn std::error::Error>>
{
    let scratch = Scratch::new("selection-opening");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let selection = grapt_event::session_message::AssistantSelection::new(
        grapt_types::AgentId::new("agt_selected").ok_or("agent")?,
        grapt_types::ProviderId::new("openai").ok_or("provider")?,
        grapt_types::ModelId::new("selected-model").ok_or("model")?,
        Some(grapt_types::ModelVariantId::new("high").ok_or("variant")?),
    );
    let mut owner = DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    )?
    .with_selection(selection.clone())?;
    drive(
        &mut owner,
        vec![DurableFact::AttemptStarted {
            attempt: attempt("pat_selected"),
        }],
    );
    let recorded = rows(&store);
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].1, "session.assistant-message.opened.2");
    let fact = grapt_event::session_message::decode_opened_v2(&recorded[0].2)?;
    assert_eq!(fact.selection(), Some(&selection));
    assert!(
        owner.with_selection(selection).is_err(),
        "an opened Step accepted replacement attribution"
    );
    Ok(())
}

/// Content before any attempt is refused rather than attributed to nothing.
#[test]
fn content_before_an_attempt_opens_is_refused() {
    let scratch = Scratch::new("no-attempt");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    match owner.commit(text_opened(0)) {
        Err(error) => assert!(
            error.reason.contains("before any attempt opened"),
            "got: {}",
            error.reason
        ),
        Ok(()) => panic!("content with no attempt must be refused"),
    }
    assert!(owner.commit(text_delta(0, "orphan")).is_err());
    assert!(rows(&store).is_empty());
}

/// Publication happens once per committed fact, and only for facts that
/// committed. The refused second terminal is NOT offered.
#[test]
fn every_committed_fact_is_offered_once_and_a_refused_one_never_is() {
    let scratch = Scratch::new("sink");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    drive(
        &mut owner,
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            text_opened(0),
            text_delta(0, "one"),
            text_closed(0),
            DurableFact::StepEnded {
                finish: FinishReason::Stop,
            },
        ],
    );
    let before = owner.sink.offered.len();
    assert_eq!(before, 5, "one offer per accepted fact");
    let _ = owner.commit(DurableFact::StepEnded {
        finish: FinishReason::Stop,
    });
    assert_eq!(
        owner.sink.offered.len(),
        before,
        "a refused fact must never be published"
    );
    // `notify` is the supervisor's post-commit call and must not double-offer.
    owner.notify(&DurableFact::StepEnded {
        finish: FinishReason::Stop,
    });
    assert_eq!(owner.sink.offered.len(), before);
}

mod capture;
mod composition;
mod resume;

mod hosted;
mod retry_discard;

mod tool_input;
