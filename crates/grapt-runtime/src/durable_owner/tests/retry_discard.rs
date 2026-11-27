//! SC-REL-02b: an unfinished block never outlives an attempt the Step retries.
use super::*;
use grapt_provider::{AttemptFailure, AttemptFragment, ProviderFailureClass};
use grapt_types::{AttemptTerminalReason, ProviderFailureClass as TerminalClass};

/// Counts discard notices, so a witness can tell a discard from a block that
/// merely never closed.
#[derive(Debug, Default)]
struct DiscardSink {
    discarded: usize,
}

impl CommittedFactSink for DiscardSink {
    fn offer(&mut self, _fact: &DurableFact) {}
    fn open_blocks_discarded(&mut self) {
        self.discarded += 1;
    }
}

fn provider_failure(class: ProviderFailureClass) -> DurableFact {
    DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::ProviderFailure {
            class,
            classification: None,
            diagnostic: None,
            message: "fixture failure".into(),
            state: state(),
        }),
    }
}

fn attempt_failed(reason: AttemptTerminalReason) -> DurableFact {
    DurableFact::AttemptFailed {
        failure: AttemptFailure::new(reason, "fixture failure"),
    }
}

fn transport_failed() -> AttemptTerminalReason {
    AttemptTerminalReason::ProviderFailure {
        class: TerminalClass::Transport,
    }
}

fn step_failed(reason: StepTerminalReason) -> DurableFact {
    DurableFact::StepFailed {
        failure: crate::owner::StepFailure::new(
            reason,
            grapt_provider::BoundedDetail::from("fixture failure"),
        ),
    }
}

/// Drive `facts` through a fresh owner; return the folded content kinds and
/// payload lengths, the terminal, and how many discards the sink saw.
fn run(label: &str, facts: Vec<DurableFact>) -> (Vec<(ContentKind, u64)>, StepTerminal, usize) {
    let scratch = Scratch::new(label);
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        DiscardSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    drive(&mut owner, facts);
    let discarded = owner.sink.discarded;
    let history = history(&store);
    let messages = match history.messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the folded history must render: {error}"),
    };
    assert_eq!(messages.len(), 1, "one Step mints exactly one message");
    let items = match &messages[0].content {
        Some(content) => content
            .items()
            .iter()
            .enumerate()
            .map(|(index, held)| {
                assert_eq!(
                    held.item.ordinal(),
                    ContentOrdinal::new(index as u64),
                    "item ordinals stay gap-free"
                );
                (held.item.kind().clone(), held.item.payload().length())
            })
            .collect(),
        None => Vec::new(),
    };
    let terminal = match &messages[0].terminal {
        Some(terminal) => terminal.clone(),
        None => panic!("the Step wrote its terminal"),
    };
    (items, terminal, discarded)
}

/// The partial reasoning and text of the dropped attempt leave no item; the
/// retry's answer is the message's only content, at the first ordinal.
#[test]
fn sc_rel_02b_a_retried_attempt_leaves_no_trace_of_its_unfinished_blocks() {
    let (items, terminal, discarded) = run(
        "retry-discard",
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            reasoning_opened(0),
            reasoning_delta(0, "partial thought"),
            text_opened(1),
            text_delta(1, "half"),
            provider_failure(ProviderFailureClass::Transport),
            attempt_failed(transport_failed()),
            DurableFact::AttemptStarted {
                attempt: attempt("pat_2"),
            },
            text_opened(0),
            text_delta(0, "answer"),
            text_closed(0),
            DurableFact::StepEnded {
                finish: FinishReason::Stop,
            },
        ],
    );
    assert_eq!(items, vec![(ContentKind::Text, 6)]);
    assert!(matches!(terminal, StepTerminal::Ended { .. }));
    assert_eq!(discarded, 1);
}

/// Edge (b): the bound refuses the last retry. The final failure stands and
/// the Step terminal resurrects no partial block.
#[test]
fn sc_rel_02b_exhausted_retries_fail_the_step_without_resurrecting_a_partial() {
    let (items, terminal, discarded) = run(
        "retry-exhausted",
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            reasoning_opened(0),
            reasoning_delta(0, "partial thought"),
            provider_failure(ProviderFailureClass::RateLimit { retry_after: None }),
            attempt_failed(AttemptTerminalReason::ProviderFailure {
                class: TerminalClass::RateLimit,
            }),
            step_failed(StepTerminalReason::ProviderFailure {
                class: TerminalClass::RateLimit,
            }),
        ],
    );
    assert_eq!(items, Vec::new());
    assert!(matches!(terminal, StepTerminal::Failed { .. }));
    assert_eq!(discarded, 1);
}

/// A no-progress stall before visible output retries like a transport drop.
#[test]
fn sc_rel_02b_a_stall_mid_reasoning_discards_the_partial_before_its_retry() {
    let (items, _, discarded) = run(
        "retry-stall",
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            reasoning_opened(0),
            reasoning_delta(0, "partial thought"),
            attempt_failed(AttemptTerminalReason::ProviderFailure {
                class: TerminalClass::NoProgressTimeout,
            }),
            DurableFact::AttemptStarted {
                attempt: attempt("pat_2"),
            },
            text_opened(0),
            text_delta(0, "answer"),
            text_closed(0),
            DurableFact::StepEnded {
                finish: FinishReason::Stop,
            },
        ],
    );
    assert_eq!(items, vec![(ContentKind::Text, 6)]);
    assert_eq!(discarded, 1);
}

/// Edge (a): an interrupt and a failure that ends the Step keep today's
/// behaviour — the Step terminal closes the partial into the failed Step.
#[test]
fn sc_rel_02b_an_interrupt_or_terminal_failure_keeps_the_partial() {
    let interrupted = AttemptTerminalReason::Interrupted {
        by: InterruptionSource::User,
    };
    let refused = AttemptTerminalReason::ProviderFailure {
        class: TerminalClass::Authentication,
    };
    for (label, failure, fragment) in [
        ("keep-interrupt", interrupted.clone(), None),
        (
            "keep-terminal",
            refused,
            Some(provider_failure(ProviderFailureClass::Authentication)),
        ),
        // The retryable failure arrived, then an interrupt settled the
        // attempt before the Step could retry it: the interrupt decides.
        (
            "keep-interrupt-after-retryable",
            interrupted,
            Some(provider_failure(ProviderFailureClass::Transport)),
        ),
    ] {
        let mut facts = vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            text_opened(0),
            text_delta(0, "partial"),
        ];
        facts.extend(fragment);
        facts.push(attempt_failed(failure));
        facts.push(step_failed(StepTerminalReason::Interrupted {
            by: InterruptionSource::User,
        }));
        let (items, terminal, discarded) = run(label, facts);
        assert_eq!(items, vec![(ContentKind::Text, 7)], "{label}");
        assert!(matches!(terminal, StepTerminal::Failed { .. }), "{label}");
        assert_eq!(discarded, 0, "{label}");
    }
}

/// Edge (c): a block that closed before the failure is visible output, so
/// the Step stops; that block is committed exactly once and the partial after
/// it is kept as the failed Step's truth.
#[test]
fn sc_rel_02b_a_closed_block_before_the_failure_commits_exactly_once() {
    let (items, terminal, discarded) = run(
        "retry-closed",
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            text_opened(0),
            text_delta(0, "shown"),
            text_closed(0),
            reasoning_opened(1),
            reasoning_delta(1, "partial"),
            provider_failure(ProviderFailureClass::Transport),
            attempt_failed(transport_failed()),
            step_failed(StepTerminalReason::ProviderFailure {
                class: TerminalClass::Transport,
            }),
        ],
    );
    assert_eq!(
        items,
        vec![(ContentKind::Text, 5), (ContentKind::Reasoning, 7)]
    );
    assert!(matches!(terminal, StepTerminal::Failed { .. }));
    assert_eq!(discarded, 0);
}

/// Process loss after a discard: the owner is dropped with no Step terminal,
/// as a crash would leave it. Folding the log twice yields the retry's item
/// alone, at the first ordinal, and no terminal for the restart to resurrect.
#[test]
fn sc_rel_02b_a_crash_after_a_discard_folds_only_committed_items() {
    let scratch = Scratch::new("retry-crash");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    {
        let mut owner = match DurableAttemptOwner::elect(
            Arc::clone(&store),
            scratch.artifacts(),
            &clock,
            &ids,
            DiscardSink::default(),
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
                text_delta(0, "half"),
                provider_failure(ProviderFailureClass::Transport),
                attempt_failed(transport_failed()),
                DurableFact::AttemptStarted {
                    attempt: attempt("pat_2"),
                },
                text_opened(0),
                text_delta(0, "answer"),
                text_closed(0),
            ],
        );
    }
    let fold = |store: &Arc<Mutex<EventStore>>| {
        let history = history(store);
        let messages = match history.messages() {
            Ok(messages) => messages,
            Err(error) => panic!("the folded history must render: {error}"),
        };
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].terminal, None, "the lost Step has no terminal");
        match &messages[0].content {
            Some(content) => content
                .items()
                .iter()
                .map(|held| {
                    (
                        held.item.ordinal(),
                        held.item.kind().clone(),
                        held.item.payload().length(),
                    )
                })
                .collect::<Vec<_>>(),
            None => Vec::new(),
        }
    };
    let first = fold(&store);
    assert_eq!(first, vec![(ContentOrdinal::FIRST, ContentKind::Text, 6)]);
    assert_eq!(fold(&store), first, "replay is deterministic");
}

/// SC-REL-02c: finished reasoning with nothing visible after it is held, so
/// a drop discards it with the unfinished text; the retry re-thinks.
#[test]
fn sc_rel_02c_a_drop_after_finished_reasoning_discards_it_and_rethinks() {
    let (items, terminal, discarded) = run(
        "retry-finished-reasoning",
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            reasoning_opened(0),
            reasoning_delta(0, "first thought"),
            reasoning_closed(0),
            text_opened(1),
            text_delta(1, "half"),
            provider_failure(ProviderFailureClass::Transport),
            attempt_failed(transport_failed()),
            DurableFact::AttemptStarted {
                attempt: attempt("pat_2"),
            },
            reasoning_opened(0),
            reasoning_delta(0, "rethought"),
            reasoning_closed(0),
            text_opened(1),
            text_delta(1, "answer"),
            text_closed(1),
            DurableFact::StepEnded {
                finish: FinishReason::Stop,
            },
        ],
    );
    assert_eq!(
        items,
        vec![(ContentKind::Reasoning, 9), (ContentKind::Text, 6)],
        "only the retry's reasoning, ahead of its answer"
    );
    assert!(matches!(terminal, StepTerminal::Ended { .. }));
    assert_eq!(discarded, 1);
}

/// Held reasoning is the failed Step's truth when nothing retries: an
/// interrupt commits it once, at the Step terminal.
#[test]
fn sc_rel_02c_an_interrupt_after_finished_reasoning_commits_it_once() {
    let (items, terminal, discarded) = run(
        "keep-finished-reasoning",
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            reasoning_opened(0),
            reasoning_delta(0, "first thought"),
            reasoning_closed(0),
            attempt_failed(AttemptTerminalReason::Interrupted {
                by: InterruptionSource::User,
            }),
            step_failed(StepTerminalReason::Interrupted {
                by: InterruptionSource::User,
            }),
        ],
    );
    assert_eq!(items, vec![(ContentKind::Reasoning, 13)]);
    assert!(matches!(terminal, StepTerminal::Failed { .. }));
    assert_eq!(discarded, 0);
}

/// Past the hold bound the reasoning is committed early, which the owner
/// reports, so the Step stops instead of retrying from a changed history.
#[test]
fn sc_rel_02c_reasoning_past_the_hold_bound_commits_and_forbids_the_retry() {
    let scratch = Scratch::new("retry-hold-bound");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        DiscardSink::default(),
        session(),
        step(1),
        None,
    ) {
        Ok(owner) => owner,
        Err(error) => panic!("the owner must elect: {error}"),
    };
    let half = "r".repeat((MAX_HELD_REASONING_BYTES / 2) as usize);
    drive(
        &mut owner,
        vec![
            DurableFact::AttemptStarted {
                attempt: attempt("pat_1"),
            },
            reasoning_opened(0),
            reasoning_delta(0, &half),
            reasoning_closed(0),
        ],
    );
    assert!(!owner.committed_output(), "within the bound it is held");
    drive(
        &mut owner,
        vec![
            reasoning_opened(1),
            reasoning_delta(1, &half),
            reasoning_delta(1, "!"),
            reasoning_closed(1),
        ],
    );
    assert!(owner.committed_output(), "past the bound it is committed");
    drive(
        &mut owner,
        vec![
            provider_failure(ProviderFailureClass::Transport),
            attempt_failed(transport_failed()),
            step_failed(StepTerminalReason::ProviderFailure {
                class: TerminalClass::Transport,
            }),
        ],
    );
    assert_eq!(owner.sink.discarded, 0);
    let history = history(&store);
    let messages = match history.messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the folded history must render: {error}"),
    };
    let kinds: Vec<_> = match &messages[0].content {
        Some(content) => content
            .items()
            .iter()
            .map(|held| held.item.kind().clone())
            .collect(),
        None => Vec::new(),
    };
    assert_eq!(kinds, vec![ContentKind::Reasoning, ContentKind::Reasoning]);
}

/// Empty finished blocks hold nothing, and tiny ones are bounded by count as
/// well as bytes: the sixteenth is held, the seventeenth forces the commit.
#[test]
fn sc_rel_02c_held_reasoning_is_bounded_by_count_and_ignores_empty_blocks() {
    let scratch = Scratch::new("retry-hold-count");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = match DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        DiscardSink::default(),
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
    let mut block = 0u32;
    for _ in 0..100 {
        drive(
            &mut owner,
            vec![reasoning_opened(block), reasoning_closed(block)],
        );
        block += 1;
    }
    assert!(
        owner.held.is_empty(),
        "an empty block commits nothing to hold"
    );
    for count in 1..=MAX_HELD_REASONING_BLOCKS {
        drive(
            &mut owner,
            vec![
                reasoning_opened(block),
                reasoning_delta(block, "r"),
                reasoning_closed(block),
            ],
        );
        block += 1;
        assert_eq!(owner.held.len(), count);
        assert!(!owner.committed_output());
    }
    drive(
        &mut owner,
        vec![
            reasoning_opened(block),
            reasoning_delta(block, "r"),
            reasoning_closed(block),
        ],
    );
    assert!(
        owner.held.is_empty(),
        "past the count bound the hold commits"
    );
    assert!(owner.committed_output());
    let history = history(&store);
    let messages = match history.messages() {
        Ok(messages) => messages,
        Err(error) => panic!("the folded history must render: {error}"),
    };
    let committed = messages[0]
        .content
        .as_ref()
        .map_or(0, |content| content.len());
    assert_eq!(committed, MAX_HELD_REASONING_BLOCKS + 1);
}
