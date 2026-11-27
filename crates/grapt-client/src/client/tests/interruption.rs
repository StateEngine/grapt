//! interruption witnesses, split from the carrier by subject.
//!
//! Moved unchanged: the ratchet bounds a FILE, and a witness that was edited
//! on its way out of one would be a different witness.

use super::*;

#[test]
fn a_rebuilt_interrupt_reads_as_interrupted_rather_than_failed() {
    // The corrected shape the oracle mints at `session/runner/llm.ts:107`:
    // the structured error carries it and `finish` is ABSENT, because an
    // interrupt is not a provider finish and that vocabulary has no member for
    // one.
    let view = rebuilt_from(vec![(None, durable_error("aborted", "Step interrupted"))]);
    assert_eq!(
        view.execution,
        ExecutionState::Settled {
            execution_id: None,
            outcome: Outcome::Interrupted
        },
        "a reconnect after an interrupt must not tell the user their own stop failed"
    );
    assert_eq!(
        view.interrupted_observed, None,
        "a DURABLE record establishes an outcome, never a live observation of one"
    );
}

#[test]
fn the_pre_repair_producer_shape_is_read_as_a_failure() {
    // THE RECORD THAT WAS INCONSISTENT WITH ITSELF, kept as a witness because
    // the rule outlives the shape.
    //
    // Before the producer repair, `dogfood/mod.rs` emitted the interrupted
    // MESSAGE while serialising the type as `"StepFailed"` and always emitting
    // a finish — so one durable row carried two truths. The retained route
    // matches on the message and showed the interrupt.
    //
    // BE EXACT ABOUT THIS CLIENT'S HALF, because the convenient version is
    // wrong: it did not read the error and disagree about the type. It
    // discarded the structured error ENTIRELY and read only `finish`, so it
    // reported a failure without ever seeing the interrupted message. The
    // record was inconsistent AND this reader was blind to the half that
    // carried the truth; the decode above is what fixed the second problem.
    //
    // A client that resolved a self-contradictory record by picking a
    // favourite half would be guessing. It stays a failure, and the rule still
    // holds now that the producer emits the corrected shape.
    let view = rebuilt_from(vec![(
        Some(crate::wire::DurableFinish::Error),
        durable_error("StepFailed", "Step interrupted"),
    )]);
    assert_eq!(
        view.execution,
        ExecutionState::Settled {
            execution_id: None,
            outcome: Outcome::Failed
        },
        "a present finish contradicts the interrupted shape and is not overridden"
    );
}

#[test]
fn a_present_finish_disqualifies_even_the_exact_interrupted_pair() {
    // The pair is exact but `finish` is present, which the target shape says it
    // must not be. Refusing this is what makes the previous witness a rule
    // rather than a coincidence of the current type string.
    let view = rebuilt_from(vec![(
        Some(crate::wire::DurableFinish::Error),
        durable_error("aborted", "Step interrupted"),
    )]);
    assert_eq!(
        view.execution,
        ExecutionState::Settled {
            execution_id: None,
            outcome: Outcome::Failed
        },
        "an inconsistent record is not read as an interrupt"
    );
}

#[test]
fn other_aborted_errors_are_not_step_interruption() {
    // `aborted` is minted for a declined permission, a suspended permission and
    // a rejected question too. Keying on the type alone would report every one
    // of them as a user interrupt, so both halves are matched.
    for message in [
        "Permission declined",
        "Tool execution interrupted",
        "Step interrupted ",
        "step interrupted",
    ] {
        let view = rebuilt_from(vec![(None, durable_error("aborted", message))]);
        assert_eq!(
            view.execution,
            ExecutionState::Settled {
                execution_id: None,
                outcome: Outcome::Failed
            },
            "`{message}` is an aborted error but it is not step interruption"
        );
    }
}

#[test]
fn a_record_with_neither_finish_nor_error_settles_nothing() {
    // Silence is not a terminal. Inventing one from an absent field is the
    // class of defect the execution-id `Option` exists to prevent.
    let view = rebuilt_from(vec![(None, None)]);
    assert_eq!(
        view.execution,
        ExecutionState::Idle,
        "an assistant row carrying no outcome must leave the projection alone"
    );
}

#[test]
fn an_interrupted_turn_is_not_resurrected_over_a_later_successful_one() {
    // A interrupted, then B succeeded, then reconciled. The LAST record is the
    // current outcome; an earlier interrupt must not be repainted over it.
    let view = rebuilt_from(vec![
        (None, durable_error("aborted", "Step interrupted")),
        (Some(crate::wire::DurableFinish::Stop), None),
    ]);
    assert_eq!(
        view.execution,
        ExecutionState::Settled {
            execution_id: None,
            outcome: Outcome::Succeeded
        },
        "the newer durable outcome stands; a stale interrupt never overwrites it"
    );
}

#[test]
fn the_exact_interrupted_message_under_a_different_type_is_not_an_interrupt() {
    // THE INDEPENDENT NEGATIVE FOR THE TYPE HALF.
    //
    // Every other near-miss varies the message while holding the type at
    // `aborted`, and the pre-repair case moves the type AND the finish — so its
    // absent-finish guard would mask a missing type comparison. Without this
    // case, dropping the type check entirely leaves the suite green.
    //
    // Here the finish is absent and the message is byte-exact, so ONLY the type
    // can refuse it.
    for kind in [
        "provider.transport",
        "StepFailed",
        "tool.result-missing",
        "",
    ] {
        let view = rebuilt_from(vec![(None, durable_error(kind, "Step interrupted"))]);
        assert_eq!(
            view.execution,
            ExecutionState::Settled {
                execution_id: None,
                outcome: Outcome::Failed
            },
            "`{kind}` is not `aborted`, so this is not step interruption however the message reads"
        );
    }
}
