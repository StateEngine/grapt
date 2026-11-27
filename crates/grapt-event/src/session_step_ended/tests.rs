//! Witnesses for the `session.step.ended.2` payload contract.
//!
//! These live in the LIB, not an integration test, because the event
//! mutation harness runs `cargo test -p grapt-event --lib <witness>`.
//! A witness an integration target owns would be invisible to it, and a
//! mutation row naming an invisible witness proves nothing.

use super::*;
use grapt_types::{ProviderCompletionDisposition, StepNumber};
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn step(n: u32) -> StepNumber {
    match StepNumber::new(n) {
        Some(step) => step,
        None => panic!("{n} is not a positive step number"),
    }
}

fn fact(completion: ProviderCompletionDisposition) -> StepEndedFact {
    StepEndedFact::new(step(1), completion)
}

fn text(fact: &StepEndedFact) -> String {
    match String::from_utf8(encode_canonical_bytes(fact)) {
        Ok(text) => text,
        Err(error) => panic!("canonical bytes must be UTF-8: {error}"),
    }
}

const EVERY_DISPOSITION: [(ProviderCompletionDisposition, &str); 4] = [
    (
        ProviderCompletionDisposition::AssertedComplete,
        "asserted-complete",
    ),
    (
        ProviderCompletionDisposition::AssertedContinue,
        "asserted-continue",
    ),
    (ProviderCompletionDisposition::Truncated, "truncated"),
    (ProviderCompletionDisposition::Unstated, "unstated"),
];

/// Every disposition survives the round trip as itself.
///
/// The table is EXHAUSTIVE over the closed vocabulary rather than a sample, and
/// its length is asserted: a fifth disposition must arrive here and be given a
/// spelling. That is the point of encoding a closed enum by tag rather than by
/// `Debug`.
#[test]
fn every_disposition_round_trips_as_itself() -> TestResult {
    assert_eq!(
        EVERY_DISPOSITION.len(),
        4,
        "the grid must cover every disposition"
    );
    for (completion, tag) in EVERY_DISPOSITION {
        let original = fact(completion);
        let encoded = encode(&original);
        assert_eq!(
            encoded,
            json!({ "step": 1, "completion": tag }),
            "{completion:?} must encode as its own tag"
        );
        let decoded = decode(&encoded)?;
        assert_eq!(decoded, original, "{completion:?} did not round-trip");
        assert_eq!(decoded.completion(), completion);
        assert_eq!(decoded.step().value(), 1);
    }
    Ok(())
}

/// The four encodings must be DISTINCT.
///
/// Without this, a codec that emitted one constant tag would satisfy every
/// round trip above, because each case would decode back to whatever that
/// single tag meant.
#[test]
fn the_four_dispositions_encode_distinctly() {
    let mut tags: Vec<&str> = EVERY_DISPOSITION.iter().map(|(_, tag)| *tag).collect();
    tags.sort_unstable();
    tags.dedup();
    assert_eq!(tags.len(), 4, "two dispositions share one spelling");
}

/// Truncated is carried, not smoothed into a completion claim.
///
/// Provider `length` maps to `Truncated`, and the mandatory rule is that it is
/// never treated as ordinary idle. A codec that let it reach the bytes as
/// `asserted-complete` would put that decision beyond the runtime's reach, so
/// the distinction is pinned in the durable bytes themselves.
#[test]
fn truncated_is_not_stored_as_a_completion_claim() {
    let truncated = text(&fact(ProviderCompletionDisposition::Truncated));
    assert!(truncated.contains("truncated"), "{truncated}");
    assert!(!truncated.contains("asserted-complete"), "{truncated}");
    assert!(
        !ProviderCompletionDisposition::Truncated.asserted_complete(),
        "truncation is never a completion assertion"
    );
}

/// The canonical bytes are the exact frozen spelling.
///
/// Pinned as a literal rather than as `encode` compared with itself: every
/// relative assertion holds just as well after the encoder changes, which is
/// exactly the change a durable byte format must not absorb silently. Key
/// order is the canonical writer's, not the struct's.
#[test]
fn canonical_bytes_are_pinned() {
    assert_eq!(
        text(&StepEndedFact::new(
            step(7),
            ProviderCompletionDisposition::AssertedComplete,
        )),
        r#"{"completion":"asserted-complete","step":7}"#
    );
}

/// A step number of zero is unrepresentable, so it cannot be decoded either.
#[test]
fn step_zero_is_refused() {
    match decode(&json!({ "step": 0, "completion": "unstated" })).err() {
        Some(PayloadError::InvalidField {
            field, expected, ..
        }) => {
            assert_eq!(field, "step");
            assert_eq!(expected, "a positive step number");
        }
        other => panic!("expected zero to be refused, got {other:?}"),
    }
}

/// Missing, null and wrong-typed fields are three DISTINCT refusals.
///
/// Collapsing them would lose the difference between a writer that never had
/// the coordinate and one that had it and erased it.
#[test]
fn absence_null_and_wrong_type_are_distinguished() {
    let cases: [(&str, Value, PayloadError); 4] = [
        (
            "missing step",
            json!({ "completion": "unstated" }),
            PayloadError::MissingField {
                at: "session.step.ended.2",
                field: "step".to_owned(),
            },
        ),
        (
            "null step",
            json!({ "step": Value::Null, "completion": "unstated" }),
            PayloadError::NullField {
                at: "session.step.ended.2",
                field: "step".to_owned(),
            },
        ),
        (
            "missing completion",
            json!({ "step": 1 }),
            PayloadError::MissingField {
                at: "session.step.ended.2",
                field: "completion".to_owned(),
            },
        ),
        (
            "numeric completion",
            json!({ "step": 1, "completion": 3 }),
            PayloadError::InvalidField {
                at: "session.step.ended.2",
                field: "completion".to_owned(),
                expected: "a string",
            },
        ),
    ];
    for (label, payload, expected) in cases {
        assert_eq!(decode(&payload).err(), Some(expected), "{label}");
    }
}

/// A payload that is not an object at all is refused before any field is read.
#[test]
fn a_non_object_payload_is_refused() {
    assert!(
        matches!(
            decode(&json!("session.step.ended.2")),
            Err(PayloadError::NotAnObject { .. })
        ),
        "a bare string is not a payload"
    );
}

/// An unknown field fails closed rather than being ignored.
#[test]
fn an_unknown_field_is_refused_rather_than_dropped() {
    match decode(&json!({ "step": 1, "completion": "unstated", "cost": 0.02 })).err() {
        Some(PayloadError::UnknownField { field, .. }) => assert_eq!(field, "cost"),
        other => panic!("expected an unreviewed coordinate to be refused, got {other:?}"),
    }
}

/// The frozen oracle's v1 payload is not this contract and cannot pass as it.
///
/// This is the whole reason the kind is `.2`. The v1 shape of the SAME name
/// carries `finish` as a raw provider reason plus `cost`, `tokens` and
/// `assistantMessageID`. Decode keys on the exact `(name, version)` pair, so
/// the two can never answer for each other; if this ever passes, a v1 row has
/// become readable as a Rust-native fact.
#[test]
fn the_frozen_v1_payload_is_not_a_v2_fact() {
    let v1 = json!({
        "sessionID": "ses_1",
        "assistantMessageID": "msg_1",
        "finish": "stop",
        "cost": 0.01,
        "tokens": { "input": 1, "output": 2 },
    });
    match decode(&v1).err() {
        Some(PayloadError::MissingField { field, .. }) => assert_eq!(field, "step"),
        other => panic!("the oracle payload is not this contract, got {other:?}"),
    }
}

/// A vocabulary this build does not share is named, not guessed.
#[test]
fn an_unknown_disposition_is_refused_by_name() {
    match decode(&json!({ "step": 1, "completion": "asserted-maybe" })).err() {
        Some(PayloadError::UnknownVariant { field, value, .. }) => {
            assert_eq!(field, "completion");
            assert_eq!(value, "asserted-maybe");
        }
        other => panic!("expected an unknown disposition, got {other:?}"),
    }
}
