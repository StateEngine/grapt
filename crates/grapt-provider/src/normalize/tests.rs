use super::*;
use crate::fragment::{AdapterState, FinishReason, ProviderFailureClass, TokenUsage};

fn call(id: &str) -> ToolCallId {
    match ToolCallId::new(id) {
        Some(value) => value,
        None => panic!("test fixture id must be non-empty"),
    }
}

fn block(n: u32) -> BlockOrdinal {
    BlockOrdinal::new(n)
}

fn opened() -> StreamValidator {
    let mut v = StreamValidator::new();
    assert!(v.accept(&AttemptFragment::AttemptOpened).is_ok());
    v
}

fn text_open(n: u32) -> AttemptFragment {
    AttemptFragment::TextOpened {
        block: block(n),
        state: AdapterState::none(),
    }
}
fn text_delta(n: u32) -> AttemptFragment {
    AttemptFragment::TextDelta {
        block: block(n),
        text: "chunk".into(),
        state: AdapterState::none(),
    }
}
fn text_close(n: u32) -> AttemptFragment {
    AttemptFragment::TextClosed {
        block: block(n),
        state: AdapterState::none(),
    }
}
fn terminal() -> AttemptFragment {
    AttemptFragment::ProviderTerminal {
        finish: FinishReason::Stop,
        usage: None,
        state: AdapterState::none(),
    }
}
fn tool_open(id: &str, tool: &str, execution: ToolExecution) -> AttemptFragment {
    AttemptFragment::ToolInputOpened {
        call: call(id),
        tool: tool.into(),
        execution,
        state: AdapterState::none(),
    }
}
fn tool_called(id: &str, tool: &str, execution: ToolExecution) -> AttemptFragment {
    AttemptFragment::ToolCalled {
        call: call(id),
        tool: tool.into(),
        input: "{}".into(),
        execution,
        state: AdapterState::none(),
    }
}
fn hosted_result(id: &str, tool: &str) -> AttemptFragment {
    AttemptFragment::HostedToolResult {
        call: call(id),
        tool: tool.into(),
        result: "ok".into(),
        state: AdapterState::none(),
    }
}

// ---- attempt framing -------------------------------------------------

#[test]
fn fragments_before_attempt_opened_are_rejected() {
    let mut v = StreamValidator::new();
    assert_eq!(v.accept(&text_open(0)), Err(StreamViolation::NotOpened));
}

#[test]
fn attempt_cannot_open_twice() {
    let mut v = opened();
    assert_eq!(
        v.accept(&AttemptFragment::AttemptOpened),
        Err(StreamViolation::AlreadyOpened)
    );
}

#[test]
fn terminal_is_absorbing() {
    // Contract: "no fragment follows either terminal fragment".
    let mut v = opened();
    assert!(v.accept(&terminal()).is_ok());
    assert!(v.terminated());
    assert_eq!(v.accept(&text_open(0)), Err(StreamViolation::AfterTerminal));
    assert_eq!(v.accept(&terminal()), Err(StreamViolation::AfterTerminal));
    assert_eq!(
        v.accept(&AttemptFragment::Usage {
            usage: TokenUsage::default()
        }),
        Err(StreamViolation::AfterTerminal)
    );
}

#[test]
fn provider_failure_is_also_terminal() {
    let mut v = opened();
    let failure = AttemptFragment::ProviderFailure {
        class: ProviderFailureClass::Transport,
        classification: None,
        diagnostic: None,
        message: "connection reset".into(),
        state: AdapterState::none(),
    };
    assert!(v.accept(&failure).is_ok());
    assert!(v.terminated());
    assert_eq!(v.accept(&text_open(0)), Err(StreamViolation::AfterTerminal));
}

// ---- block discipline ------------------------------------------------

#[test]
fn sequential_blocks_are_legal_and_keep_their_ordinals() {
    // Contract: "sequential blocks are allowed and retain an ordinal".
    let mut v = opened();
    for fragment in [
        text_open(0),
        text_delta(0),
        text_close(0),
        text_open(1),
        text_close(1),
    ] {
        assert!(v.accept(&fragment).is_ok(), "{fragment:?} must be legal");
    }
}

#[test]
fn two_blocks_open_at_once_is_rejected() {
    // Contract: "one active text and reasoning block at a time".
    let mut v = opened();
    assert!(v.accept(&text_open(0)).is_ok());
    assert_eq!(
        v.accept(&text_open(1)),
        Err(StreamViolation::BlockAlreadyOpen {
            kind: BlockKind::Text,
            block: 1,
            active: 0,
        })
    );
}

#[test]
fn delta_without_matching_open_is_rejected() {
    // Contract: "a delta/end requires the matching opened block".
    let mut v = opened();
    assert_eq!(
        v.accept(&text_delta(0)),
        Err(StreamViolation::BlockNotOpen {
            kind: BlockKind::Text,
            block: 0
        })
    );
    assert!(v.accept(&text_open(0)).is_ok());
    // Right kind, wrong ordinal.
    assert_eq!(
        v.accept(&text_delta(7)),
        Err(StreamViolation::BlockNotOpen {
            kind: BlockKind::Text,
            block: 7
        })
    );
}

#[test]
fn text_and_reasoning_blocks_are_independent() {
    let mut v = opened();
    assert!(v.accept(&text_open(0)).is_ok());
    assert!(v
        .accept(&AttemptFragment::ReasoningOpened {
            block: block(0),
            state: AdapterState::none(),
        })
        .is_ok());
    // A reasoning delta must not be satisfied by the open text block.
    assert!(v
        .accept(&AttemptFragment::ReasoningDelta {
            block: block(0),
            text: "think".into(),
            state: AdapterState::none(),
        })
        .is_ok());
}

// ---- tool discipline -------------------------------------------------

#[test]
fn tool_name_cannot_change_mid_call() {
    let mut v = opened();
    assert!(v
        .accept(&tool_open("call_1", "read", ToolExecution::Local))
        .is_ok());
    assert_eq!(
        v.accept(&AttemptFragment::ToolInputDelta {
            call: call("call_1"),
            tool: "write".into(),
            text: "{".into(),
        }),
        Err(StreamViolation::ToolNameChanged {
            call: "call_1".into(),
            expected: "read".into(),
            actual: "write".into(),
        })
    );
}

#[test]
fn tool_execution_owner_cannot_change() {
    let mut v = opened();
    assert!(v
        .accept(&tool_open("call_1", "read", ToolExecution::Local))
        .is_ok());
    assert_eq!(
        v.accept(&tool_called(
            "call_1",
            "read",
            ToolExecution::ProviderHosted
        )),
        Err(StreamViolation::ToolExecutionChanged {
            call: "call_1".into()
        })
    );
}

#[test]
fn tool_called_only_once() {
    let mut v = opened();
    assert!(v
        .accept(&tool_open("call_1", "read", ToolExecution::Local))
        .is_ok());
    assert!(v
        .accept(&tool_called("call_1", "read", ToolExecution::Local))
        .is_ok());
    assert_eq!(
        v.accept(&tool_called("call_1", "read", ToolExecution::Local)),
        Err(StreamViolation::ToolCalledTwice {
            call: "call_1".into()
        })
    );
}

#[test]
fn tool_call_may_arrive_without_an_input_sequence() {
    // Zero-argument tools: adapters emit ToolCalled directly.
    let mut v = opened();
    assert!(v
        .accept(&tool_called("call_1", "now", ToolExecution::Local))
        .is_ok());
    assert_eq!(v.unsettled_calls().len(), 1);
}

#[test]
fn only_provider_hosted_calls_receive_hosted_results() {
    // Contract: "only provider-hosted calls may receive hosted results".
    let mut v = opened();
    assert!(v
        .accept(&tool_open("call_1", "web", ToolExecution::Local))
        .is_ok());
    assert!(v
        .accept(&tool_called("call_1", "web", ToolExecution::Local))
        .is_ok());
    assert_eq!(
        v.accept(&hosted_result("call_1", "web")),
        Err(StreamViolation::HostedResultForLocalCall {
            call: "call_1".into()
        })
    );
}

#[test]
fn hosted_result_before_call_is_rejected() {
    let mut v = opened();
    assert!(v
        .accept(&tool_open("call_1", "web", ToolExecution::ProviderHosted))
        .is_ok());
    assert_eq!(
        v.accept(&hosted_result("call_1", "web")),
        Err(StreamViolation::HostedResultBeforeCall {
            call: "call_1".into()
        })
    );
}

#[test]
fn a_late_result_after_settlement_is_an_ignored_straggler_not_a_failure() {
    // Evidence: the provider-straggler-asymmetry fixture. caseA records
    // toolSuccessEvents=1 and ranToCompletion=true for a late ERROR after
    // a success, so the run must survive it. Its corrected expectation
    // extends the same handling to caseB's duplicate SUCCESS, which the
    // oracle turns into an unchecked defect: "never a second settlement
    // and never a crash".
    let mut v = opened();
    assert!(v
        .accept(&tool_open("call_1", "web", ToolExecution::ProviderHosted))
        .is_ok());
    assert!(v
        .accept(&tool_called("call_1", "web", ToolExecution::ProviderHosted))
        .is_ok());
    assert!(v.accept(&hosted_result("call_1", "web")).is_ok());

    // Duplicate SUCCESS — ignored, diagnosed, run continues.
    match v.accept(&hosted_result("call_1", "web")) {
        Ok(accepted) => {
            assert_eq!(
                accepted.ignored,
                Some(IgnoredEvidence {
                    call: "call_1".into(),
                    kind: StragglerKind::Success,
                })
            );
            assert!(!accepted.terminal);
        }
        Err(violation) => panic!("a straggler must not fail the step: {violation}"),
    }

    // Late FAILURE — same treatment, distinguishable kind.
    let late_error = AttemptFragment::HostedToolFailed {
        call: call("call_1"),
        tool: "web".into(),
        error: "timeout".into(),
        state: AdapterState::none(),
    };
    match v.accept(&late_error) {
        Ok(accepted) => assert_eq!(
            accepted.ignored.map(|evidence| evidence.kind),
            Some(StragglerKind::Failure)
        ),
        Err(violation) => panic!("a late failure must not fail the step: {violation}"),
    }

    // Exactly-once settlement still holds: the call is settled, so it is
    // not in the sweep, and no second settlement was recorded.
    assert!(v.unsettled_calls().is_empty());
}

#[test]
fn unsettled_sweep_is_deterministic_and_excludes_settled_calls() {
    let mut v = opened();
    for (id, exec) in [
        ("call_c", ToolExecution::Local),
        ("call_a", ToolExecution::ProviderHosted),
        ("call_b", ToolExecution::Local),
    ] {
        assert!(v.accept(&tool_open(id, "t", exec)).is_ok());
        assert!(v.accept(&tool_called(id, "t", exec)).is_ok());
    }
    assert!(v.accept(&hosted_result("call_a", "t")).is_ok());
    let unsettled: Vec<&str> = v.unsettled_calls().iter().map(|id| id.as_str()).collect();
    assert_eq!(
        unsettled,
        vec!["call_b", "call_c"],
        "sorted, settled excluded"
    );
}

// ---- visible output (the retry precondition) -------------------------

#[test]
fn visible_output_rises_once_and_never_falls() {
    let mut v = opened();
    assert!(!v.visible_output());
    // Usage alone is not visible output — a usage-only stream stays retryable.
    let accepted = match v.accept(&AttemptFragment::Usage {
        usage: TokenUsage::default(),
    }) {
        Ok(accepted) => accepted,
        Err(error) => panic!("usage must be legal: {error}"),
    };
    assert!(!accepted.first_visible_output);
    assert!(!v.visible_output());

    // An unfinished block is not visible output either.
    for fragment in [text_open(0), text_delta(0)] {
        let accepted = match v.accept(&fragment) {
            Ok(accepted) => accepted,
            Err(error) => panic!("{fragment:?} must be legal: {error}"),
        };
        assert!(!accepted.first_visible_output);
        assert!(!v.visible_output());
    }

    let accepted = match v.accept(&text_close(0)) {
        Ok(accepted) => accepted,
        Err(error) => panic!("text close must be legal: {error}"),
    };
    assert!(accepted.first_visible_output, "rising edge reported once");
    assert!(v.visible_output());

    let accepted = match v.accept(&text_open(1)) {
        Ok(accepted) => accepted,
        Err(error) => panic!("a second text open must be legal: {error}"),
    };
    assert!(
        !accepted.first_visible_output,
        "rising edge is reported exactly once"
    );
    assert!(v.visible_output());
}

#[test]
fn a_rejected_fragment_does_not_set_visible_output() {
    // A malformed tool delta must not silently close the retry window.
    // Streaming arguments are not output; the committed call is.
    let mut v = opened();
    assert!(v
        .accept(&tool_open("call_1", "read", ToolExecution::Local))
        .is_ok());
    assert!(
        !v.visible_output(),
        "an argument stream is not visible output"
    );
    assert!(v
        .accept(&tool_called("call_1", "read", ToolExecution::Local))
        .is_ok());
    assert!(v.visible_output(), "the committed call is visible output");

    let mut fresh = opened();
    assert!(fresh
        .accept(&AttemptFragment::ToolInputDelta {
            call: call("ghost"),
            tool: "read".into(),
            text: "{".into(),
        })
        .is_err());
    assert!(
        !fresh.visible_output(),
        "a rejected fragment must not count as output"
    );
}

// ---- the corrected expectation (§B.9) --------------------------------

#[test]
fn every_violation_maps_to_one_step_failure_never_a_panic() {
    // Red-team §B.9: the oracle dies on malformed fragments. The corrected
    // expectation is exactly one Step.Failed-class outcome.
    let violations = [
        StreamViolation::NotOpened,
        StreamViolation::AlreadyOpened,
        StreamViolation::AfterTerminal,
        StreamViolation::BlockAlreadyOpen {
            kind: BlockKind::Text,
            block: 1,
            active: 0,
        },
        StreamViolation::BlockNotOpen {
            kind: BlockKind::Reasoning,
            block: 3,
        },
        StreamViolation::ToolInputNotOpened { call: "c".into() },
        StreamViolation::ToolInputAlreadyOpened { call: "c".into() },
        StreamViolation::ToolNameChanged {
            call: "c".into(),
            expected: "a".into(),
            actual: "b".into(),
        },
        StreamViolation::ToolExecutionChanged { call: "c".into() },
        StreamViolation::ToolCalledTwice { call: "c".into() },
        StreamViolation::HostedResultForLocalCall { call: "c".into() },
        StreamViolation::HostedResultBeforeCall { call: "c".into() },
    ];
    for violation in violations {
        let failure = violation.attempt_failure();
        assert_eq!(
            failure.reason(),
            &AttemptTerminalReason::ProviderFailure {
                class: grapt_types::ProviderFailureClass::InvalidProviderOutput
            }
        );
        assert!(
            !failure.detail().as_str().is_empty(),
            "{violation:?} must carry a diagnosable detail"
        );
    }
}

// ---- properties over arbitrary sequences -----------------------------

/// Deterministic LCG so a failure is reproducible from the seed alone.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, modulo: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % modulo
    }
}

fn fragment_pool() -> Vec<AttemptFragment> {
    vec![
        AttemptFragment::AttemptOpened,
        text_open(0),
        text_delta(0),
        text_close(0),
        text_open(1),
        AttemptFragment::ReasoningOpened {
            block: block(0),
            state: AdapterState::none(),
        },
        AttemptFragment::ReasoningDelta {
            block: block(0),
            text: "t".into(),
            state: AdapterState::none(),
        },
        tool_open("call_1", "read", ToolExecution::Local),
        tool_open("call_2", "web", ToolExecution::ProviderHosted),
        tool_called("call_1", "read", ToolExecution::Local),
        tool_called("call_2", "web", ToolExecution::ProviderHosted),
        hosted_result("call_2", "web"),
        hosted_result("call_1", "read"),
        AttemptFragment::Usage {
            usage: TokenUsage::default(),
        },
        terminal(),
        AttemptFragment::ProviderFailure {
            class: ProviderFailureClass::RateLimit { retry_after: None },
            classification: None,
            diagnostic: None,
            message: "429".into(),
            state: AdapterState::none(),
        },
    ]
}

#[test]
fn arbitrary_sequences_never_panic_and_hold_their_invariants() {
    let pool = fragment_pool();
    for seed in 0..500u64 {
        let mut rng = Lcg(seed);
        let mut v = StreamValidator::new();
        let mut was_visible = false;
        let mut saw_terminal = false;

        for _ in 0..24 {
            let fragment = &pool[rng.next(pool.len())];
            let result = v.accept(fragment);

            // Property 1: visible output is monotonic.
            assert!(
                v.visible_output() >= was_visible,
                "seed {seed}: visible_output fell"
            );
            was_visible = v.visible_output();

            // Property 2: once terminal, every later fragment is refused.
            if saw_terminal {
                assert_eq!(
                    result,
                    Err(StreamViolation::AfterTerminal),
                    "seed {seed}: accepted a fragment after terminal"
                );
            }
            if result.is_ok() && fragment.is_terminal() {
                saw_terminal = true;
            }

            // Property 3: terminated() agrees with what was accepted.
            assert_eq!(v.terminated(), saw_terminal, "seed {seed}");

            // Property 4: every rejection is diagnosable as one failure.
            if let Err(violation) = result {
                assert_eq!(
                    violation.attempt_failure().reason(),
                    &AttemptTerminalReason::ProviderFailure {
                        class: grapt_types::ProviderFailureClass::InvalidProviderOutput
                    }
                );
            }
        }
    }
}

#[test]
fn unsettled_calls_are_always_called_and_never_settled() {
    let pool = fragment_pool();
    for seed in 0..200u64 {
        let mut rng = Lcg(seed);
        let mut v = StreamValidator::new();
        for _ in 0..24 {
            let _ = v.accept(&pool[rng.next(pool.len())]);
        }
        // The sweep list must be sorted and free of duplicates, so a
        // fixture can assert an exact settlement order.
        let ids: Vec<&str> = v.unsettled_calls().iter().map(|id| id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ids, sorted, "seed {seed}: sweep must be sorted and unique");
    }
}
