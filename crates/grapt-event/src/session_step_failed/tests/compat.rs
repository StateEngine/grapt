//! Compatibility witnesses and the failure-reason vocabulary grid, split out
//! of `tests.rs` for the new-module line bound. Like the parent module these
//! live in the LIB, not an integration test, because the event mutation
//! harness runs `cargo test -p grapt-event --lib <witness>`; a witness an
//! integration target owns would be invisible to it.

use super::super::{decode, encode, encode_canonical_bytes, StepFailedFact};
use super::{every_failure_reason, fact, TestResult};
use grapt_types::{ProviderFailureClass, StepTerminalReason};
use serde_json::{json, Value};

/// Frozen predecessor witness (dev 8687ca8b): how an older build decodes the
/// class word of a `provider-failure` reason. The vocabulary is copied
/// verbatim from 8687ca8b's `class_of`; do NOT add arms here — an extended
/// fixture is no longer the old reader. The unknown-word fallback is the live
/// decoder's path, which this change does not touch (see
/// an_unknown_nested_vocabulary_value_decodes_to_its_path_qualified_tag).
mod frozen_predecessor_8687ca8b {
    use grapt_types::{ProviderFailureClass, StepTerminalReason};

    /// 8687ca8b's class vocabulary, verbatim: eleven words, no more.
    pub fn class_of(raw: &str) -> Option<ProviderFailureClass> {
        match raw {
            "rate-limit" => Some(ProviderFailureClass::RateLimit),
            "provider-internal" => Some(ProviderFailureClass::ProviderInternal),
            "transport" => Some(ProviderFailureClass::Transport),
            "no-progress-timeout" => Some(ProviderFailureClass::NoProgressTimeout),
            "authentication" => Some(ProviderFailureClass::Authentication),
            "quota" => Some(ProviderFailureClass::Quota),
            "content-policy" => Some(ProviderFailureClass::ContentPolicy),
            "invalid-request" => Some(ProviderFailureClass::InvalidRequest),
            "invalid-provider-output" => Some(ProviderFailureClass::InvalidProviderOutput),
            "unknown-provider" => Some(ProviderFailureClass::UnknownProvider),
            "no-route" => Some(ProviderFailureClass::NoRoute),
            _ => None,
        }
    }

    /// The old reader's provider-failure reason level over that vocabulary.
    pub fn decode_provider_failure(raw_class: &str) -> StepTerminalReason {
        match class_of(raw_class) {
            Some(class) => StepTerminalReason::ProviderFailure { class },
            None => StepTerminalReason::Unknown {
                tag: super::super::super::unknown_reason_tag(
                    "provider-failure",
                    "class",
                    raw_class,
                ),
            },
        }
    }
}

/// Old reader, new payload: the new code writes the terminal record, and the
/// frozen 8687ca8b decoder reads it back as a qualified Unknown — the older
/// build still reads the Session, naming a word it does not have.
#[test]
fn an_old_reader_reads_the_new_local_refusal_record_as_qualified_unknown() -> TestResult {
    let fact = fact(StepTerminalReason::ProviderFailure {
        class: ProviderFailureClass::LocalRefusal,
    });
    let payload = encode(&fact)?;
    let word = payload
        .pointer("/reason/class")
        .and_then(Value::as_str)
        .ok_or("the new record must carry its class word")?;
    let decoded = frozen_predecessor_8687ca8b::decode_provider_failure(word);
    assert_eq!(
        decoded,
        StepTerminalReason::Unknown {
            tag: "provider-failure/class/local-refusal".to_owned()
        },
        "an older build must read the new record as qualified Unknown, not refuse it"
    );
    Ok(())
}

/// New reader, old payload: the record an older build wrote for a class the
/// vocabulary still shares decodes unchanged. The literal is the 8687ca8b
/// encoding of a provider-failure / invalid-provider-output step record.
#[test]
fn a_new_reader_reads_the_predecessor_invalid_provider_output_record_unchanged() -> TestResult {
    let old_payload = json!({
        "step": 1,
        "reason": { "kind": "provider-failure", "class": "invalid-provider-output" }
    });
    let decoded = decode(&old_payload)?;
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::ProviderFailure {
            class: ProviderFailureClass::InvalidProviderOutput
        },
        "old records keep their meaning in the new build"
    );
    Ok(())
}

#[test]
fn every_failure_reason_round_trips() -> TestResult {
    let cases = every_failure_reason();
    // EQUALITY, not a lower bound. `>= 15` let a removed case be masked by an
    // added one, and an under-covered grid still looked full — which is how
    // the principal level stayed unreached while this test passed.
    assert_eq!(
        cases.len(),
        18,
        "the grid must cover every failure arm and every nested vocabulary"
    );
    for (label, reason) in cases {
        let original = fact(reason.clone());
        let decoded = decode(&encode(&original)?)?;
        assert_eq!(decoded, original, "{label} did not round-trip");
        assert_eq!(decoded.reason(), &reason, "{label} lost its reason");
        assert_eq!(decoded.step().value(), 1, "{label} lost its step number");
    }
    Ok(())
}

/// The encodings must be DISTINCT. Without this, a codec that emitted one
/// constant tag would satisfy every round-trip above, because each case would
/// decode back to whatever the single tag meant.
#[test]
fn no_two_reasons_share_an_encoding() {
    let mut seen: Vec<String> = every_failure_reason()
        .into_iter()
        .map(|(_, reason)| crate_canonical(&fact(reason)))
        .collect();
    let total = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!(
        seen.len(),
        total,
        "two distinct terminal reasons encode to the same bytes"
    );
}

fn crate_canonical(fact: &StepFailedFact) -> String {
    match encode_canonical_bytes(fact) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => panic!("the fixture fact must encode: {error}"),
    }
}
