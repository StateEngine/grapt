//! Witnesses for the `session.step.failed.2` payload contract.
//!
//! These live in the LIB, not an integration test, because the event
//! mutation harness runs `cargo test -p grapt-event --lib <witness>`.
//! A witness an integration target owns would be invisible to it, and a
//! mutation row naming an invisible witness proves nothing.

use super::*;
use grapt_types::{
    AccountId, AgentId, AmbiguityEvidence, AuthorityPrincipal, ContextFailure, ExternalEffectState,
    HookId, InterruptionSource, LayerName, PolicyVersion, ProviderCompletionDisposition,
    ProviderFailureClass, StepNumber, StepTerminalReason,
};
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn step(n: u32) -> StepNumber {
    match StepNumber::new(n) {
        Some(step) => step,
        None => unreachable!("the fixture step number is a positive literal"),
    }
}

fn policy(name: &str) -> PolicyVersion {
    match PolicyVersion::new(name) {
        Some(policy) => policy,
        None => unreachable!("the fixture policy version is a non-empty literal"),
    }
}

fn fact(reason: StepTerminalReason) -> StepFailedFact {
    match StepFailedFact::new(step(1), reason) {
        Ok(fact) => fact,
        Err(error) => unreachable!("the fixture reason is a failure: {error}"),
    }
}

/// Every reason the durable fact may carry, one per closed variant.
///
/// Nine of the ten `StepTerminalReason` variants appear (the tenth, `Ended`, is
/// refused and has its own witnesses). Six of these nine have NO producer
/// anywhere in the workspace today; they are here as CODEC coverage, so that
/// the encoding exists and round-trips before a producer needs it. That is a
/// property of the codec and explicitly NOT a claim that anything writes them.
///
/// The nested vocabularies are covered EXHAUSTIVELY, not by one sample each:
/// all five `AuthorityPrincipal` arms and all three `ExternalEffectState`
/// arms appear. `AutomatedReviewer` is the arm the R1 review found
/// constructed nowhere in this crate; without it the principal level had no
/// executable presence at all, so neither its tag spelling nor its `agent`
/// coordinate was pinned by anything.
#[expect(clippy::too_many_lines, reason = "legacy")]
fn every_failure_reason() -> Vec<(&'static str, StepTerminalReason)> {
    vec![
        (
            "model-asserted-complete",
            StepTerminalReason::ModelAssertedComplete,
        ),
        (
            "policy-limit",
            StepTerminalReason::PolicyLimit {
                policy: policy("secode-runtime/step-policy/ruled-default/1"),
            },
        ),
        (
            "budget-exhausted",
            StepTerminalReason::BudgetExhausted {
                policy: policy("budget/1"),
            },
        ),
        (
            "interrupted",
            StepTerminalReason::Interrupted {
                by: InterruptionSource::User,
            },
        ),
        (
            "interrupted-managed-shutdown",
            StepTerminalReason::Interrupted {
                by: InterruptionSource::ManagedShutdown,
            },
        ),
        (
            "provider-failure",
            StepTerminalReason::ProviderFailure {
                class: ProviderFailureClass::NoProgressTimeout,
            },
        ),
        (
            "provider-failure-local-refusal",
            StepTerminalReason::ProviderFailure {
                class: ProviderFailureClass::LocalRefusal,
            },
        ),
        (
            "continuity-unavailable",
            StepTerminalReason::ContextFailure {
                failure: ContextFailure::ContinuityUnavailable,
            },
        ),
        (
            "context-failure",
            StepTerminalReason::ContextFailure {
                failure: ContextFailure::CompactionNoProgress,
            },
        ),
        (
            "authority-refused-human",
            StepTerminalReason::AuthorityRefused {
                principal: AuthorityPrincipal::Human {
                    account: match AccountId::new("acct_1") {
                        Some(account) => account,
                        None => unreachable!("non-empty literal"),
                    },
                },
            },
        ),
        (
            "authority-refused-system",
            StepTerminalReason::AuthorityRefused {
                principal: AuthorityPrincipal::System,
            },
        ),
        (
            "authority-refused-policy-layer",
            StepTerminalReason::AuthorityRefused {
                principal: AuthorityPrincipal::PolicyLayer {
                    layer: match LayerName::new("project") {
                        Some(layer) => layer,
                        None => unreachable!("non-empty literal"),
                    },
                },
            },
        ),
        (
            "authority-refused-hook",
            StepTerminalReason::AuthorityRefused {
                principal: AuthorityPrincipal::Hook {
                    hook: match HookId::new("hook_1") {
                        Some(hook) => hook,
                        None => unreachable!("non-empty literal"),
                    },
                },
            },
        ),
        (
            "authority-refused-automated-reviewer",
            StepTerminalReason::AuthorityRefused {
                principal: AuthorityPrincipal::AutomatedReviewer {
                    agent: match AgentId::new("agt_1") {
                        Some(agent) => agent,
                        None => unreachable!("non-empty literal"),
                    },
                },
            },
        ),
        ("durability-failure", StepTerminalReason::DurabilityFailure),
        (
            "process-loss-none-initiated",
            StepTerminalReason::ProcessLoss {
                effect: ExternalEffectState::NoneInitiated,
            },
        ),
        (
            "process-loss-settled",
            StepTerminalReason::ProcessLoss {
                effect: ExternalEffectState::Settled,
            },
        ),
        (
            "process-loss-ambiguous",
            StepTerminalReason::ProcessLoss {
                effect: ExternalEffectState::Ambiguous {
                    evidence: match AmbiguityEvidence::new("request sent; no terminal observed") {
                        Some(evidence) => evidence,
                        None => unreachable!("non-empty literal"),
                    },
                },
            },
        ),
    ]
}

/// A repeat is written as `compaction-no-progress`: a
/// build that predates the typed repeat still reads the Session it fails in.
#[test]
fn a_repeated_compaction_is_written_as_no_progress() -> TestResult {
    let tag = context_tag(ContextFailure::CompactionRepeated);
    assert_eq!(tag, "compaction-no-progress");
    assert_eq!(context_of(tag), Some(ContextFailure::CompactionNoProgress));
    Ok(())
}

// ---------------------------------------------------------------------------
// `Ended` is not a failure
// ---------------------------------------------------------------------------

/// THE RULED REFUSAL. A Step that ended successfully is `session.step.ended`.
#[test]
fn the_constructor_refuses_ended_as_a_failure() {
    for completion in [
        ProviderCompletionDisposition::AssertedComplete,
        ProviderCompletionDisposition::AssertedContinue,
        ProviderCompletionDisposition::Truncated,
        ProviderCompletionDisposition::Unstated,
    ] {
        assert_eq!(
            StepFailedFact::new(step(1), StepTerminalReason::Ended { completion }).err(),
            Some(FactError::NotAFailure),
            "no completion disposition makes `Ended` a failure"
        );
    }
}

/// Decode refuses it too, and refuses it as a CONSTRUCTOR refusal rather than
/// as an unknown variant. The two are different facts: "this build does not
/// know that word" versus "that word is known and is not a failure". A reader
/// that conflated them could not tell a version skew from an illegal write.
#[test]
fn decode_refuses_ended_as_a_constructor_refusal_not_an_unknown_variant() {
    let payload = json!({
        "step": 1,
        "reason": { "kind": "ended", "completion": "asserted-complete" }
    });
    assert_eq!(
        decode(&payload).err(),
        Some(PayloadError::RefusedByConstructor {
            failure: FactError::NotAFailure
        })
    );
}

/// THE CONTROL for the test above, now in the tolerant direction: an
/// actually-unknown reason kind decodes to `Unknown` carrying the word.
/// Without this, a decoder that refused every unknown word would pass the
/// ended test and break a version-skewed reader.
#[test]
fn an_unknown_reason_kind_decodes_to_unknown_carrying_its_word() {
    let payload = json!({ "step": 1, "reason": { "kind": "aborted" } });
    let decoded = match decode(&payload) {
        Ok(decoded) => decoded,
        Err(error) => panic!("an unknown kind is vocabulary, not corruption: {error:?}"),
    };
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::Unknown {
            tag: "aborted".to_owned()
        },
        "the word is carried raw, byte-capped, for diagnosis"
    );
}

// ---------------------------------------------------------------------------
// Fail-closed, one test per refusal category
// ---------------------------------------------------------------------------

fn valid() -> Value {
    json!({
        "step": 2,
        "reason": { "kind": "interrupted", "by": "user" }
    })
}

/// The control for the whole refusal grid: the shape the refusals are derived
/// from must itself be ACCEPTED, or every refusal below could be firing for an
/// unrelated reason.
#[test]
fn the_grid_base_payload_is_accepted() -> TestResult {
    let decoded = decode(&valid())?;
    assert_eq!(decoded.step().value(), 2);
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::Interrupted {
            by: InterruptionSource::User
        }
    );
    Ok(())
}

#[test]
fn a_non_object_payload_is_refused() {
    for value in [json!(null), json!(7), json!("step"), json!([])] {
        assert!(
            matches!(decode(&value), Err(PayloadError::NotAnObject { .. })),
            "{value} is not an object"
        );
    }
}

#[test]
fn a_missing_field_is_refused() {
    for field in ["step", "reason"] {
        let mut payload = valid();
        if let Some(object) = payload.as_object_mut() {
            object.remove(field);
        }
        match decode(&payload).err() {
            Some(PayloadError::MissingField { field: got, .. }) => assert_eq!(got, field),
            other => panic!("removing `{field}` must be a missing field, got {other:?}"),
        }
    }
}

/// Missing and explicit `null` are DIFFERENT defects: a writer that never had
/// the coordinate versus one that had it and erased it.
#[test]
fn an_explicit_null_is_refused_and_is_not_a_missing_field() {
    for field in ["step", "reason"] {
        let mut payload = valid();
        if let Some(object) = payload.as_object_mut() {
            object.insert(field.to_owned(), Value::Null);
        }
        match decode(&payload).err() {
            Some(PayloadError::NullField { field: got, .. }) => assert_eq!(got, field),
            other => panic!("a null `{field}` must be a null field, got {other:?}"),
        }
    }
}

#[test]
fn a_wrong_type_is_refused() {
    let mut payload = valid();
    if let Some(object) = payload.as_object_mut() {
        object.insert("step".to_owned(), Value::from("2"));
    }
    assert!(matches!(
        decode(&payload),
        Err(PayloadError::InvalidField { .. })
    ));

    let mut payload = valid();
    if let Some(object) = payload.as_object_mut() {
        object.insert("reason".to_owned(), Value::from("interrupted"));
    }
    assert!(matches!(
        decode(&payload),
        Err(PayloadError::NotAnObject { .. })
    ));
}

/// A Step number is POSITIVE by type. Zero is the one a naive `as_u64` would
/// accept, so it is named explicitly alongside the other wrong values.
#[test]
fn a_non_positive_or_oversized_step_is_refused() {
    for bad in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!(u64::from(u32::MAX) + 1),
    ] {
        let mut payload = valid();
        if let Some(object) = payload.as_object_mut() {
            object.insert("step".to_owned(), bad.clone());
        }
        match decode(&payload).err() {
            Some(PayloadError::InvalidField { field, .. }) => assert_eq!(field, "step"),
            other => panic!("step {bad} must be refused, got {other:?}"),
        }
    }
}

/// Unknown extra fields are IGNORED at every level: a future writer may add
/// an optional coordinate, and this reader projects the fact it shares.
///
/// "Every level" is EXACTLY the four the codec declares — the root, the
/// reason, the principal and the effect — and each case asserts the decoded
/// TYPED reason survives the foreign field, so a decoder that started
/// refusing (or silently dropping the known fact) at any level fails here.
#[test]
fn unknown_extra_fields_are_ignored_at_every_level() -> TestResult {
    // Root level.
    let mut payload = valid();
    if let Some(object) = payload.as_object_mut() {
        object.insert("sessionID".to_owned(), Value::from("ses_1"));
    }
    let decoded = decode(&payload)?;
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::Interrupted {
            by: InterruptionSource::User
        },
        "a root-level extra field must not disturb the shared fact"
    );

    // Reason level.
    let payload = json!({
        "step": 1,
        "reason": { "kind": "interrupted", "by": "user", "message": "stopped" }
    });
    let decoded = decode(&payload)?;
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::Interrupted {
            by: InterruptionSource::User
        },
        "a reason-level extra field must not disturb the shared fact"
    );

    // Principal level: only a payload that actually reaches this level
    // proves it, the way the old refusal grid did.
    let payload = json!({
        "step": 1,
        "reason": {
            "kind": "authority-refused",
            "principal": {
                "kind": "automated-reviewer",
                "agent": "agt_1",
                "reviewer": "seat-b"
            }
        }
    });
    let decoded = decode(&payload)?;
    let agent = match AgentId::new("agt_1") {
        Some(agent) => agent,
        None => unreachable!("non-empty literal"),
    };
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::AuthorityRefused {
            principal: AuthorityPrincipal::AutomatedReviewer { agent }
        },
        "a principal-level extra field must not disturb the shared fact"
    );

    // Effect level.
    let payload = json!({
        "step": 1,
        "reason": {
            "kind": "process-loss",
            "effect": { "kind": "settled", "detail": "x" }
        }
    });
    let decoded = decode(&payload)?;
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::ProcessLoss {
            effect: ExternalEffectState::Settled
        },
        "an effect-level extra field must not disturb the shared fact"
    );
    Ok(())
}

/// A known reason's REQUIRED coordinates stay required: `interrupted` without
/// its `by` is corruption even in a tolerant reader. The foreign extra
/// coordinate, however, is vocabulary: it is ignored, and the shared fact
/// projects.
#[test]
fn a_known_reason_keeps_its_required_fields_and_ignores_foreign_ones() -> TestResult {
    let payload = json!({
        "step": 1,
        "reason": { "kind": "interrupted", "class": "rate-limit" }
    });
    // The `by` it does need is missing, which is the first fault reached.
    match decode(&payload).err() {
        Some(PayloadError::MissingField { field, .. }) => assert_eq!(field, "by"),
        other => panic!("expected the missing `by`, got {other:?}"),
    }

    let payload = json!({
        "step": 1,
        "reason": { "kind": "interrupted", "by": "user", "class": "rate-limit" }
    });
    let decoded = decode(&payload)?;
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::Interrupted {
            by: InterruptionSource::User
        },
        "the foreign coordinate is ignored; the shared fact projects"
    );
    Ok(())
}

#[test]
fn an_unknown_nested_vocabulary_value_decodes_to_its_path_qualified_tag() -> TestResult {
    let cases = [
        (
            json!({ "kind": "interrupted", "by": "timeout" }),
            "interrupted/by/timeout",
        ),
        (
            json!({ "kind": "provider-failure", "class": "provider-failure" }),
            "provider-failure/class/provider-failure",
        ),
        (
            json!({ "kind": "context-failure", "failure": "aborted" }),
            "context-failure/failure/aborted",
        ),
    ];
    for (reason, tag) in cases {
        let payload = json!({ "step": 1, "reason": reason });
        let decoded = decode(&payload)?;
        assert_eq!(
            decoded.reason(),
            &StepTerminalReason::Unknown {
                tag: tag.to_owned()
            },
            "the tag names the kind, the field, then the word"
        );
    }
    Ok(())
}

/// The principal level owns its OWN vocabulary, and an unrecognised kind
/// decodes to `Unknown` at the principal level rather than resolving to a
/// neighbouring arm.
///
/// `System` is the specific wrong answer this pins. It is the one principal
/// arm carrying no payload, so a fallback to it decodes cleanly and quietly
/// relabels an unknown authority as the system's own — a durable record of
/// who refused, naming the wrong principal, with nothing malformed to see.
///
/// The second half is the CONTROL: the real spelling still decodes to the
/// real principal. Without it, a build that swallowed every principal kind
/// would satisfy the first half and look correct.
#[test]
fn an_unknown_principal_kind_decodes_to_unknown_at_its_own_level() -> TestResult {
    let payload = json!({
        "step": 1,
        "reason": {
            "kind": "authority-refused",
            "principal": { "kind": "reviewer" }
        }
    });
    let decoded = decode(&payload)?;
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::Unknown {
            tag: "authority-refused/principal/kind/reviewer".to_owned()
        },
        "the tag names the reason, the level, and the unknown word — never a          neighbouring arm"
    );

    let payload = json!({
        "step": 1,
        "reason": {
            "kind": "authority-refused",
            "principal": { "kind": "automated-reviewer", "agent": "agt_1" }
        }
    });
    let decoded = decode(&payload)?;
    let agent = match AgentId::new("agt_1") {
        Some(agent) => agent,
        None => unreachable!("non-empty literal"),
    };
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::AuthorityRefused {
            principal: AuthorityPrincipal::AutomatedReviewer { agent }
        }
    );
    Ok(())
}

/// Empty evidence is not evidence. The type refuses it, and the codec reports a
/// wrong VALUE rather than letting a blank string become absence.
#[test]
fn empty_ambiguity_evidence_is_refused_as_a_value_not_absence() {
    let payload = json!({
        "step": 1,
        "reason": {
            "kind": "process-loss",
            "effect": { "kind": "ambiguous", "evidence": "" }
        }
    });
    match decode(&payload).err() {
        Some(PayloadError::InvalidField { field, .. }) => assert_eq!(field, "evidence"),
        other => panic!("empty evidence must be refused, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// v1 and v2 cannot answer for each other
// ---------------------------------------------------------------------------

/// The frozen v1 payload — including its bare `error.type: "aborted"`, the exact
/// carrier R2a removed — is refused by the v2 codec. Decode keys on the exact
/// `(name, version)` pair, so this is belt-and-braces at the payload level.
#[test]
fn the_frozen_v1_payload_does_not_decode_as_v2() {
    let v1 = json!({
        "sessionID": "ses_033601c26ffe",
        "assistantMessageID": "msg_033601c26ffe",
        "error": { "type": "aborted", "message": "Step interrupted" },
        "cost": 0,
        "tokens": {}
    });
    assert!(
        decode(&v1).is_err(),
        "a v1 payload must never decode as a v2 failure fact"
    );
}

/// And the reverse: a v2 payload has none of v1's required fields, so a v1
/// reader cannot mistake it for its own shape either.
#[test]
fn a_v2_payload_carries_none_of_the_v1_required_fields() -> TestResult {
    let encoded = encode(&fact(StepTerminalReason::Interrupted {
        by: InterruptionSource::User,
    }))?;
    let object = match encoded.as_object() {
        Some(object) => object,
        None => panic!("the encoding is an object"),
    };
    for forbidden in [
        "sessionID",
        "assistantMessageID",
        "error",
        "cost",
        "tokens",
        "snapshot",
        "files",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "v2 must not carry the v1 coordinate `{forbidden}`: session identity is the \
             envelope aggregate and usage has its own vocabulary"
        );
    }
    // Exactly the two ruled coordinates, and nothing else.
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["reason", "step"]);
    Ok(())
}

/// The diagnostic is excluded by ruling. Nothing in the encoding carries free
/// text except the closed vocabularies' own tags and the identities they name.
#[test]
fn no_diagnostic_text_field_exists_anywhere_in_the_encoding() -> TestResult {
    for (label, reason) in every_failure_reason() {
        let encoded = encode(&fact(reason))?;
        let text = encoded.to_string();
        for forbidden in ["\"detail\"", "\"message\"", "\"diagnostic\"", "\"text\""] {
            assert!(
                !text.contains(forbidden),
                "{label} encoded a diagnostic field {forbidden}; R2b ruling 1 excludes it"
            );
        }
    }
    Ok(())
}

/// Canonical bytes are deterministic and hashable — inherited from the crate's
/// canonical writer rather than re-invented here.
#[test]
fn canonical_bytes_are_stable_and_key_ordered() -> TestResult {
    let subject = fact(StepTerminalReason::ProviderFailure {
        class: ProviderFailureClass::Quota,
    });
    let first = encode_canonical_bytes(&subject)?;
    let second = encode_canonical_bytes(&subject)?;
    assert_eq!(first, second, "canonical bytes must be deterministic");
    let text = String::from_utf8_lossy(&first);
    assert!(
        text.find("\"reason\"") < text.find("\"step\""),
        "canonical form orders keys, got {text}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Envelope provenance: the second Rust-native kind inherits the law
// ---------------------------------------------------------------------------

/// The envelope needed NO new arm for this kind: `EnvelopeKind` admits
/// `RustSession` iff the kind's provenance is `RustNative`, so registering the
/// row made the law apply automatically. "Automatically" is a claim, and this
/// is the measurement — proved in BOTH directions for `session.step.failed.2`
/// specifically, not inherited from the compaction kind's grid.
#[test]
fn the_v2_kind_rides_only_the_rust_session_variant() -> TestResult {
    use crate::{
        AggregateId, EnvelopeError, EnvelopeKind, EventEnvelopeV2, EventKind, KindProvenance,
    };
    use grapt_types::{EventId, EventSeq, SessionId, Timestamp, WriterEpoch};
    let native = EventKind::SessionStepFailedV2;
    assert_eq!(native.provenance(), KindProvenance::RustNative);

    let aggregate = AggregateId::Session("ses_033601c26ffe".parse::<SessionId>()?);
    let id = "evt_033601c26ffe".parse::<EventId>()?;
    let epoch = match WriterEpoch::new(1) {
        Some(epoch) => epoch,
        None => unreachable!("one is a positive epoch"),
    };
    let bytes = encode_canonical_bytes(&fact(StepTerminalReason::Interrupted {
        by: InterruptionSource::User,
    }))?;
    let build = |kind: EnvelopeKind| {
        EventEnvelopeV2::new(
            id.clone(),
            aggregate.clone(),
            EventSeq::new(1),
            epoch,
            kind,
            Timestamp::from_millis(1_786_147_200_000),
            bytes.clone(),
        )
    };

    // Accepted on its own variant.
    let accepted = build(EnvelopeKind::RustSession(native))?;
    assert_eq!(accepted.kind().kind_str(), "session.step.failed.2");

    // Refused on Legacy, as a PROVENANCE mismatch naming expected and actual.
    // A bare refusal would not say which law fired.
    assert_eq!(
        build(EnvelopeKind::Legacy(native)).err(),
        Some(EnvelopeError::ProvenanceMismatch {
            kind: native,
            // `expected` is what the VARIANT requires, `actual` is what the
            // KIND carries. Legacy requires Manifest; this kind is RustNative.
            expected: KindProvenance::Manifest,
            actual: KindProvenance::RustNative,
        })
    );

    // THE OTHER DIRECTION, and the control: v1 of the same NAME is Manifest, so
    // it rides Legacy and is refused on RustSession. Without this half, a build
    // that admitted everything on both variants would pass the arm above.
    let legacy = EventKind::SessionStepFailedV1;
    assert_eq!(legacy.provenance(), KindProvenance::Manifest);
    let accepted_legacy = build(EnvelopeKind::Legacy(legacy))?;
    assert_eq!(accepted_legacy.kind().kind_str(), "session.step.failed.1");
    assert_eq!(
        build(EnvelopeKind::RustSession(legacy)).err(),
        Some(EnvelopeError::ProvenanceMismatch {
            kind: legacy,
            expected: KindProvenance::RustNative,
            actual: KindProvenance::Manifest,
        })
    );
    Ok(())
}

#[test]
fn scc_continuity_unavailable_replays_as_context_failure() -> TestResult {
    let original = fact(StepTerminalReason::ContextFailure {
        failure: ContextFailure::ContinuityUnavailable,
    });
    let encoded = encode(&original)?;
    assert!(encoded.to_string().contains("continuity-unavailable"));
    assert_eq!(decode(&encoded)?, original);
    Ok(())
}

// ---------------------------------------------------------------------------
// The tolerant contract, witnessed
// ---------------------------------------------------------------------------

/// THE CONTRACT, in one test: three classes of input, three answers.
///
/// (a) corruption is refused; (b) a known non-failure is refused as a
/// constructor fact; (c) an unknown word decodes to `Unknown`. A reader that
/// conflates any two of the three fails one of the three arms.
#[test]
fn the_three_classes_of_input_get_three_answers() -> TestResult {
    // (a) corruption: a known reason missing a required coordinate.
    let corrupt = json!({ "step": 1, "reason": { "kind": "interrupted" } });
    assert!(
        matches!(decode(&corrupt), Err(PayloadError::MissingField { .. })),
        "a known reason with a broken required coordinate is corruption"
    );

    // (b) a known non-failure: refused by the constructor, not by vocabulary.
    let ended = json!({
        "step": 1,
        "reason": { "kind": "ended", "completion": "asserted-complete" }
    });
    assert_eq!(
        decode(&ended).err(),
        Some(PayloadError::RefusedByConstructor {
            failure: FactError::NotAFailure
        }),
        "the kind `ended` is known, and it is not a failure"
    );

    // (c) an unknown word: vocabulary, decoded to `Unknown`.
    let unknown = json!({ "step": 1, "reason": { "kind": "governance-hold" } });
    let decoded = decode(&unknown)?;
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::Unknown {
            tag: "governance-hold".to_owned()
        },
        "the word is carried, not refused"
    );
    Ok(())
}

/// The `ended` refusal holds EVEN when the completion word is unknown to this
/// build: the kind is the fact, and the kind is not a failure.
#[test]
fn ended_is_refused_even_when_its_completion_word_is_unknown() {
    for word in ["asserted-complete", "asserted-forever"] {
        let payload = json!({ "step": 1, "reason": { "kind": "ended", "completion": word } });
        assert_eq!(
            decode(&payload).err(),
            Some(PayloadError::RefusedByConstructor {
                failure: FactError::NotAFailure
            }),
            "an unknown completion word does not make `ended` a failure"
        );
    }
}

/// THE DECODE-ONLY LAW, in its three witnesses: decode of an unknown word
/// succeeds; the writer's constructor refuses `Unknown`; and re-encoding a
/// decoded `Unknown` fact refuses rather than panicking or writing.
#[test]
fn the_decode_only_reason_decodes_and_refuses_the_constructor_and_the_encoder() -> TestResult {
    // Witness one: decode succeeds.
    let payload = json!({ "step": 3, "reason": { "kind": "quarantine-expired" } });
    let decoded = decode(&payload)?;
    assert_eq!(decoded.step().value(), 3);

    // Witness two: the writer's constructor refuses it.
    let unknown_reason = StepTerminalReason::Unknown {
        tag: "quarantine-expired".to_owned(),
    };
    assert_eq!(
        StepFailedFact::new(step(1), unknown_reason.clone()).err(),
        Some(FactError::DecodeOnly),
        "no writer in this build may mint a decode-only reason"
    );

    // Witness three: re-encoding the decoded fact refuses, fallibly.
    let refusal = encode(&decoded).err();
    assert_eq!(
        refusal,
        Some(FactError::DecodeOnly),
        "a decoded fact is projected, never written back"
    );
    assert!(encode_canonical_bytes(&decoded).is_err());
    Ok(())
}

/// The tag is byte-capped on a CHAR boundary: a long ASCII word loses its
/// tail, and a multibyte word that straddles the cap boundary is not cut in
/// the middle of a character.
#[test]
fn a_long_unknown_word_is_capped_on_a_char_boundary() -> TestResult {
    let long = "k".repeat(200);
    let payload = json!({ "step": 1, "reason": { "kind": long } });
    let decoded = decode(&payload)?;
    let tag_of = |fact: &StepFailedFact, what: &str| match fact.reason() {
        StepTerminalReason::Unknown { tag } => tag.clone(),
        other => panic!("{what} must decode to Unknown, got {other:?}"),
    };
    let tag = tag_of(&decoded, "the long word");
    assert_eq!(tag.len(), 64, "the cap is 64 bytes");
    assert!(tag.chars().all(|c| c == 'k'));

    // A multibyte word (3-byte chars) straddling byte 64: the cut must land
    // on a char boundary, so 21 whole characters (63 bytes), not 21.33.
    let boundary = "中".repeat(40);
    let payload = json!({ "step": 1, "reason": { "kind": boundary } });
    let decoded = decode(&payload)?;
    let tag = tag_of(&decoded, "the boundary word");
    assert_eq!(tag.len(), 63, "the cut lands on a char boundary");
    assert_eq!(tag.as_str(), "中".repeat(21));
    Ok(())
}

/// The tag keeps what it witnessed, RAW: a word carrying a terminal escape
/// and a newline is stored byte-for-byte (within the cap). Sanitization is
/// the display layer's job, witnessed there — the codec must not rewrite it.
#[test]
fn a_tag_carries_control_characters_raw_for_the_display_layer() -> TestResult {
    let hostile = "\u{1b}[31mred\n";
    let payload = json!({ "step": 1, "reason": { "kind": hostile } });
    let decoded = decode(&payload)?;
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::Unknown {
            tag: hostile.to_owned()
        },
        "the codec witnesses the raw word; it does not sanitize"
    );
    Ok(())
}

/// A blank is NO word, and no terminal evidence is built from one. Every
/// REQUIRED vocabulary word — the root reason kind, each known kind's nested
/// word, and the principal and effect kinds — refuses a blank as a broken
/// coordinate of a record this build shares, in BOTH supported terminal
/// versions, instead of decoding to an `Unknown` tag with no content.
///
/// Red-to-green witness for the HOLD TR-H1 blank probe: before the
/// correction all six sites decoded `Ok` on a blank. The control at the end
/// pins the other half of the law: a NON-empty unknown word still decodes
/// to `Unknown` — the refusal is a blank rule, not a closed vocabulary.
#[test]
fn a_blank_required_word_is_refused_as_corruption_in_every_version() -> TestResult {
    let payloads = [
        json!({ "step": 1, "reason": { "kind": "" } }),
        json!({ "step": 1, "reason": { "kind": "interrupted", "by": "" } }),
        json!({ "step": 1, "reason": { "kind": "provider-failure", "class": "" } }),
        json!({ "step": 1, "reason": { "kind": "context-failure", "failure": "" } }),
        json!({
            "step": 1,
            "reason": { "kind": "authority-refused", "principal": { "kind": "" } }
        }),
        json!({ "step": 1, "reason": { "kind": "process-loss", "effect": { "kind": "" } } }),
    ];
    let paths = [
        "session.step.failed.2.reason",
        "session.step.failed.2.reason",
        "session.step.failed.2.reason",
        "session.step.failed.2.reason",
        "session.step.failed.2.reason.principal",
        "session.step.failed.2.reason.effect",
    ];
    let fields = ["kind", "by", "class", "failure", "kind", "kind"];
    // v2: each blank coordinate is corruption, named at its own path.
    for ((payload, at), field) in payloads.iter().zip(paths).zip(fields) {
        match decode(payload).err() {
            Some(PayloadError::InvalidField {
                at: got_at,
                field: got_field,
                ..
            }) => {
                assert_eq!(got_at, at);
                assert_eq!(got_field, field);
            }
            other => panic!("a blank {field} must be corruption, got {other:?}"),
        }
    }
    // v3: the same refusal through the observed door; genuine completedAt
    for ((payload, _), field) in payloads.iter().zip(paths).zip(fields) {
        let mut payload = payload.clone();
        payload["completedAt"] = json!(300);
        match crate::session_step_observed::decode_failed(&payload).err() {
            Some(crate::session_step_observed::PayloadError::Terminal(_)) => {}
            other => panic!(
                "v3 must refuse the blank {field} as the terminal's own fault, got {other:?}"
            ),
        }
    }
    // Control: a non-empty unknown word is vocabulary, still `Unknown`.
    let decoded = decode(&json!({
        "step": 1,
        "reason": { "kind": "interrupted", "by": "user-pause" }
    }))?;
    assert_eq!(
        decoded.reason(),
        &StepTerminalReason::Unknown {
            tag: "interrupted/by/user-pause".to_owned()
        },
        "the refusal is a blank rule; a real word stays vocabulary"
    );
    Ok(())
}

// The compatibility witnesses and the reason-vocabulary grid live in the
// `compat` submodule, split out for the new-module line bound.
mod compat;
