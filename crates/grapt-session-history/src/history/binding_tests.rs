//! CONTEXT-QUALITY-01 r2: a retried Step whose successful attempt only called
//! tools binds that attempt to the Step message through its tool terminals.

use super::*;
use grapt_event::session_tool_terminal::{encode, ToolTerminalFact};
use grapt_types::{ToolCallId, ToolTerminalOutcome};

fn call(raw: &str) -> ToolCallId {
    ToolCallId::new(raw).unwrap_or_else(|| panic!("call identity is non-empty"))
}

fn terminal(seq: u64, at: &str, id: &str) -> Row {
    Row {
        seq,
        kind_name: "session.tool.terminal",
        kind_version: 1,
        data: encode(&ToolTerminalFact::new(
            attempt(at),
            call(id),
            ToolTerminalOutcome::Completed { output: None },
        )),
    }
}

fn usage(seq: u64, at: &str, input: u64) -> Row {
    Row {
        seq,
        kind_name: "session.usage.observed",
        kind_version: 2,
        data: sm::encode_observed(&UsageObservedFact::new(
            attempt(at),
            AttemptUsage::observed(
                UsageField::ABSENT,
                reported(input),
                reported(1),
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageObservationPhase::Terminal,
            ),
        )),
    }
}

#[test]
fn cq_a_tool_only_retry_carries_its_usage_to_the_step_message() {
    let rows = [
        // The refused attempt opened the message and reported nothing.
        opened(1, "msg_retry", "pat_refused", 1),
        appended(
            2,
            "msg_retry",
            0,
            ContentKind::Tool {
                call: call("call_retry"),
            },
        ),
        usage(3, "pat_retry", 20),
        terminal(4, "pat_retry", "call_retry"),
        // A compaction summary attempt reports usage but calls no tool.
        usage(5, "pat_summary", 99),
        ended(6, 1),
    ];
    let first = messages(&folded(&rows));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].usage_attempt, attempt("pat_retry"));
    assert_eq!(first[0].usage.len(), 1);
    // A restart folds the same rows to the same projection.
    assert_eq!(messages(&folded(&rows)), first);
}

#[test]
fn cq_a_terminal_never_rebinds_another_messages_attempt() {
    let rows = [
        opened(1, "msg_first", "pat_one", 1),
        appended(
            2,
            "msg_first",
            0,
            ContentKind::Tool {
                call: call("call_first"),
            },
        ),
        ended(3, 1),
        opened(4, "msg_second", "pat_two", 2),
        appended(
            5,
            "msg_second",
            0,
            ContentKind::Tool {
                call: call("call_second"),
            },
        ),
        // Evidence naming an attempt already bound elsewhere binds nothing.
        terminal(6, "pat_two", "call_first"),
        ended(7, 2),
    ];
    let folded = messages(&folded(&rows));
    assert_eq!(folded[0].usage_attempt, attempt("pat_one"));
    assert_eq!(folded[1].usage_attempt, attempt("pat_two"));
}

#[test]
fn cq_evidence_for_an_unknown_or_nested_call_binds_nothing() {
    let rows = [
        opened(1, "msg_only", "pat_only", 1),
        terminal(2, "pat_nested", "call_never_appended"),
        usage(3, "pat_nested", 5),
        ended(4, 1),
    ];
    let folded = messages(&folded(&rows));
    assert_eq!(folded[0].usage_attempt, attempt("pat_only"));
    assert!(folded[0].usage.is_empty());
    assert!(!store::OWNED_KINDS
        .iter()
        .any(|owned| store::BINDING_EVIDENCE_KINDS.contains(owned)));
}
