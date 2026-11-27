//! Witnesses for the three DF0-SESSION Rust-native payload contracts.
//!
//! Every test here is paired: the positive round trip, and the exact refusal
//! that a defect in the same field produces. A round trip alone would pass
//! against a decoder that accepted anything.

use super::*;
use grapt_types::{
    PolicyVersion, TokenCount, UsageField, UsageObservationPhase, UsageProtocolFamily,
    UsageProvenance,
};
use serde_json::json;

#[test]
fn block_opening_round_trips_and_refuses_corrupt_coordinates() {
    for kind in [ContentBlockKind::Text, ContentBlockKind::Reasoning] {
        let fact = ContentBlockOpenedFact::new(attempt(), message(), u32::MAX, kind);
        let encoded = encode_block_opened(&fact);
        assert_eq!(decode_block_opened(&encoded), Ok(fact));
        for (field, value) in [
            ("attempt", json!("")),
            ("message", json!("ses_wrong")),
            ("block", json!(u64::from(u32::MAX) + 1)),
            ("block", json!(-1)),
            ("kind", json!("provider_shape")),
            ("extra", json!(true)),
        ] {
            let mut corrupt = encoded.clone();
            corrupt[field] = value;
            assert!(
                decode_block_opened(&corrupt).is_err(),
                "accepted corrupt {field}"
            );
        }
    }
}

#[test]
fn a_text_checkpoint_round_trips_and_refuses_corrupt_or_inline_fields() {
    use super::checkpoint::{decode_checkpointed, encode_checkpointed, ContentCheckpointedFact};
    let fact = ContentCheckpointedFact::new(attempt(), message(), u32::MAX, reference());
    let encoded = encode_checkpointed(&fact);
    assert_eq!(decode_checkpointed(&encoded), Ok(fact));
    for (field, value) in [
        ("attempt", json!("")),
        ("message", json!("ses_wrong")),
        ("block", json!(u64::from(u32::MAX) + 1)),
        ("text", json!("the streamed text, inline")),
        ("text", json!({"id": "art_1", "length": 12})),
        ("bytes", json!("the streamed text, inline")),
    ] {
        let mut corrupt = encoded.clone();
        corrupt[field] = value;
        assert!(
            decode_checkpointed(&corrupt).is_err(),
            "accepted corrupt {field}"
        );
    }
}

fn attempt() -> PhysicalAttemptId {
    match PhysicalAttemptId::new("pat_1") {
        Some(attempt) => attempt,
        None => panic!("pat_1 is a non-empty attempt identity"),
    }
}

fn step() -> StepNumber {
    match StepNumber::new(1) {
        Some(step) => step,
        None => panic!("1 is a positive step number"),
    }
}

fn message() -> MessageId {
    match "msg_df0a".parse() {
        Ok(message) => message,
        Err(error) => panic!("msg_df0a is a valid message identity: {error}"),
    }
}

fn reference() -> ArtifactRef {
    let id = match ArtifactId::new("art_1") {
        Some(id) => id,
        None => panic!("art_1 is a non-empty artifact identity"),
    };
    let digest = match ArtifactDigest::new("sha256:aa") {
        Some(digest) => digest,
        None => panic!("sha256:aa is a non-empty digest"),
    };
    ArtifactRef::new(id, 12, digest)
}

fn count(value: u64) -> TokenCount {
    match TokenCount::new(value) {
        Ok(count) => count,
        Err(error) => panic!("fixture token count is bounded: {error}"),
    }
}

fn reported(value: u64) -> UsageField {
    UsageField::reported(count(value))
}

/// A content fact the caller asserts is well formed. The constructor is
/// fallible because a disposition can contradict its payload; a witness that is
/// not ABOUT that refusal says so here rather than repeating the match.
fn appended(item: NormalizedContentItem, disposition: CaptureDisposition) -> ContentAppendedFact {
    match ContentAppendedFact::new(message(), item, disposition) {
        Ok(fact) => fact,
        Err(error) => panic!("the fact must be constructible: {error}"),
    }
}

#[test]
fn an_opened_message_round_trips_with_its_step_and_attempt() {
    let fact = AssistantMessageOpenedFact::new(attempt(), step(), message());
    let encoded = encode_opened(&fact);
    match decode_opened(&encoded) {
        Ok(decoded) => assert_eq!(decoded, fact),
        Err(error) => panic!("the encoder's own output must decode: {error}"),
    }
}

/// A Step number of zero is not a small Step, it is an absent one.
#[test]
fn an_opened_message_refuses_step_zero() {
    let refusal = decode_opened(&json!({
        "attempt": "pat_1",
        "step": 0,
        "message": "msg_df0a",
    }));
    assert_eq!(
        refusal,
        Err(PayloadError::InvalidField {
            at: OPENED,
            field: "step".to_owned(),
            expected: "a positive step number",
        })
    );
}

/// The message identity is PARSED, not merely non-empty: an unprefixed token
/// would otherwise become a durable message address no read model can key.
#[test]
fn an_opened_message_refuses_an_unprefixed_identity() {
    let refusal = decode_opened(&json!({
        "attempt": "pat_1",
        "step": 1,
        "message": "df0a",
    }));
    assert_eq!(
        refusal,
        Err(PayloadError::InvalidField {
            at: OPENED,
            field: "message".to_owned(),
            expected: "a validated msg_* identity",
        })
    );
}

#[test]
fn each_content_kind_round_trips_at_its_own_ordinal() {
    let call = match ToolCallId::new("call_1") {
        Some(call) => call,
        None => panic!("call_1 is a non-empty call identity"),
    };
    let kinds = [
        ContentKind::Text,
        ContentKind::Reasoning,
        ContentKind::Tool { call },
        ContentKind::Opaque,
    ];
    for (index, kind) in kinds.into_iter().enumerate() {
        let ordinal = ContentOrdinal::new(index as u64);
        let fact = appended(
            NormalizedContentItem::new(ordinal, kind, reference()),
            CaptureDisposition::Complete,
        );
        let encoded = encode_appended(&fact);
        match decode_appended(&encoded) {
            Ok(decoded) => {
                assert_eq!(decoded, fact);
                assert_eq!(decoded.item().ordinal(), ordinal);
            }
            Err(error) => panic!("content item {index} must decode: {error}"),
        }
    }
}

/// A tool item without its call identity loses the pairing the ordered
/// sequence exists to preserve, so it is refused rather than decoded as an
/// unattributed item.
#[test]
fn a_tool_content_item_refuses_a_missing_call() {
    let refusal = decode_appended(&json!({
        "message": "msg_df0a",
        "item": {
            "ordinal": 0,
            "kind": "tool",
            "payload": {"id": "art_1", "length": 12, "digest": "sha256:aa"},
            "disposition": "complete",
        },
    }));
    assert_eq!(
        refusal,
        Err(PayloadError::MissingField {
            at: APPENDED_ITEM,
            field: "call".to_owned(),
        })
    );
}

/// A text item may not smuggle a `call`: unknown fields fail closed, so a
/// tool pairing cannot be attached to a kind that has none.
#[test]
fn a_text_content_item_refuses_a_smuggled_call() {
    let refusal = decode_appended(&json!({
        "message": "msg_df0a",
        "item": {
            "ordinal": 0,
            "kind": "text",
            "call": "call_1",
            "payload": {"id": "art_1", "length": 12, "digest": "sha256:aa"},
            "disposition": "complete",
        },
    }));
    assert_eq!(
        refusal,
        Err(PayloadError::UnknownField {
            at: APPENDED_ITEM,
            field: "call".to_owned(),
        })
    );
}

/// Inline bytes are unnameable: the payload is a reference and nothing else.
#[test]
fn a_content_payload_refuses_inline_text() {
    let refusal = decode_appended(&json!({
        "message": "msg_df0a",
        "item": {
            "ordinal": 0,
            "kind": "text",
            "payload": "hello",
            "disposition": "complete",
        },
    }));
    assert_eq!(
        refusal,
        Err(PayloadError::NotAnObject {
            at: APPENDED_PAYLOAD
        })
    );
}

#[test]
fn reported_usage_round_trips_with_every_product_field_present() {
    let fact = UsageObservedFact::new(
        attempt(),
        AttemptUsage::observed(
            reported(13),
            reported(10),
            reported(0),
            reported(3),
            reported(2),
            reported(1),
            reported(13),
            UsageObservationPhase::Terminal,
        ),
    );
    let encoded = encode_observed(&fact);
    match decode_observed(&encoded) {
        Ok(decoded) => assert_eq!(decoded, fact),
        Err(error) => panic!("reported usage must decode: {error}"),
    }
}

/// The protocol family is an optional v2 key: every tag round-trips to the
/// same stamp (the bytes carry the tag, not a provider name), an unstamped
/// record encodes without the key, and an unknown or null tag degrades to
/// the unstamped record instead of refusing the row, the leaves intact.
#[test]
fn the_protocol_family_key_round_trips_and_degrades_without_refusing() {
    for (family, tag) in [
        (
            UsageProtocolFamily::FreshInputPartition,
            "fresh-input-partition",
        ),
        (
            UsageProtocolFamily::InclusiveInputNoCache,
            "inclusive-input-no-cache",
        ),
        (UsageProtocolFamily::InclusiveInput, "inclusive-input"),
    ] {
        let fact = UsageObservedFact::new(
            attempt(),
            AttemptUsage::observed_stamped(
                UsageField::ABSENT,
                reported(8),
                reported(3),
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageObservationPhase::Terminal,
                family,
            ),
        );
        let encoded = encode_observed(&fact);
        let Some(usage) = encoded.get("usage").and_then(Value::as_object) else {
            panic!("usage is an object");
        };
        assert_eq!(usage.get("protocol"), Some(&json!(tag)));
        let decoded = decode_observed(&encoded)
            .unwrap_or_else(|error| panic!("stamped usage must decode: {error}"));
        assert_eq!(decoded, fact);
    }
    let fact = UsageObservedFact::new(
        attempt(),
        AttemptUsage::observed(
            UsageField::ABSENT,
            reported(8),
            reported(3),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::Terminal,
        ),
    );
    let encoded = encode_observed(&fact);
    let Some(usage) = encoded.get("usage").and_then(Value::as_object) else {
        panic!("usage is an object");
    };
    assert!(!usage.contains_key("protocol"));
    let decoded = decode_observed(&encoded)
        .unwrap_or_else(|error| panic!("unstamped usage must decode: {error}"));
    assert_eq!(decoded, fact);
    assert_eq!(decoded.usage().protocol(), None);
    // The stamp is advisory to accounting: an unknown tag and a null tag
    // are two more spellings of "not stamped", and the row must stay
    // readable.
    for protocol in [json!("inclusive-input-maybe"), json!(null)] {
        let decoded = decode_observed(&json!({
            "attempt": "pat_1",
            "usage": {
                "inputFreshTokens": {"count": 8, "provenance": "provider-reported"},
                "outputTokens": {"count": 3, "provenance": "provider-reported"},
                "protocol": protocol,
                "phase": "terminal",
            },
        }))
        .unwrap_or_else(|error| panic!("an odd protocol tag must not refuse the row: {error}"));
        assert_eq!(decoded.usage().protocol(), None);
        assert_eq!(decoded.usage().input_tokens.value(), Some(8));
        assert_eq!(decoded.usage().output_tokens.value(), Some(3));
    }
}

/// The row this whole type exists for: an ABSENT field survives the round trip
/// as absent, and an OBSERVED ZERO survives as zero. If the encoder wrote a
/// zero for absent, this test reds on the first assertion; if the decoder
/// mapped zero to absent, it reds on the second.
#[test]
fn an_absent_usage_field_never_round_trips_into_a_zero() {
    let absent = UsageObservedFact::new(
        attempt(),
        AttemptUsage::observed(
            UsageField::ABSENT,
            UsageField::ABSENT,
            reported(0),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::DuringStream,
        ),
    );
    let encoded = encode_observed(&absent);
    // The absent field is OMITTED from the bytes, not written as null or zero.
    let usage = match encoded.get("usage").and_then(Value::as_object) {
        Some(usage) => usage,
        None => panic!("usage is an object"),
    };
    assert!(!usage.contains_key("inputTokens"));
    assert!(!usage.contains_key("reasoningTokens"));
    assert!(!usage.contains_key("cacheReadInputTokens"));
    assert!(!usage.contains_key("cacheWriteInputTokens"));
    assert_eq!(
        usage.get("outputTokens"),
        Some(&json!({"count": 0, "provenance": "provider-reported"}))
    );
    match decode_observed(&encoded) {
        Ok(decoded) => {
            assert!(decoded.usage().input_tokens.is_absent());
            assert_eq!(decoded.usage().output_tokens.value(), Some(0));
            assert!(decoded.usage().reasoning_tokens.is_absent());
            assert!(decoded.usage().cache_read_input_tokens.is_absent());
            assert!(decoded.usage().cache_write_input_tokens.is_absent());
        }
        Err(error) => panic!("absent-bearing usage must decode: {error}"),
    }
}

/// Null is a THIRD spelling of "not observed", and two spellings reconcile
/// differently the moment one is mistaken for a value. It is refused.
#[test]
fn a_null_usage_field_is_refused_rather_than_read_as_absent() {
    let refusal = decode_observed(&json!({
        "attempt": "pat_1",
        "usage": {
            "inputFreshTokens": null,
            "phase": "terminal",
        },
    }));
    assert_eq!(
        refusal,
        Err(PayloadError::NullField {
            at: OBSERVED_V2_USAGE,
            field: "inputFreshTokens".to_owned(),
        })
    );
}

/// Estimated usage carries its exact policy version. Without one it is not
/// estimated-under-a-policy, it is an unattributable guess.
#[test]
fn estimated_usage_refuses_a_missing_policy_version() {
    let refusal = decode_observed(&json!({
        "attempt": "pat_1",
        "usage": {
            "outputTokens": {"count": 4, "provenance": "harness-estimated"},
            "phase": "reconciled",
        },
    }));
    assert_eq!(
        refusal,
        Err(PayloadError::MissingField {
            at: OBSERVED_V2_FIELD,
            field: "policy".to_owned(),
        })
    );
}

/// Reported is never presented as estimated: the two provenances encode to
/// different bytes and a policy version cannot ride along with a reported
/// observation.
#[test]
fn reported_usage_refuses_a_policy_version() {
    let refusal = decode_observed(&json!({
        "attempt": "pat_1",
        "usage": {
            "outputTokens": {
                "count": 4,
                "provenance": "provider-reported",
                "policy": "pol_1"
            },
            "phase": "terminal",
        },
    }));
    assert_eq!(
        refusal,
        Err(PayloadError::UnknownField {
            at: OBSERVED_V2_FIELD,
            field: "policy".to_owned(),
        })
    );
}

#[test]
fn estimated_usage_round_trips_with_its_policy_version() {
    let policy = match PolicyVersion::new("pol_1") {
        Some(policy) => policy,
        None => panic!("pol_1 is a non-empty policy version"),
    };
    let fact = UsageObservedFact::new(
        attempt(),
        AttemptUsage::observed(
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::estimated(count(7), policy),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::Reconciled,
        ),
    );
    let encoded = encode_observed(&fact);
    match decode_observed(&encoded) {
        Ok(decoded) => assert_eq!(decoded, fact),
        Err(error) => panic!("estimated usage must decode: {error}"),
    }
}

#[test]
fn legacy_v1_input_decodes_only_into_the_ambiguous_migration_slot() {
    let decoded = decode_observed_v1(&json!({
        "attempt": "pat_1",
        "usage": {
            "inputTokens": 10,
            "outputTokens": 2,
            "phase": "terminal",
            "provenance": "provider-reported"
        }
    }));
    let decoded = match decoded {
        Ok(decoded) => decoded,
        Err(error) => panic!("the shipped v1 payload must remain readable: {error}"),
    };
    assert_eq!(decoded.usage().input_tokens_ambiguous().value(), Some(10));
    assert_eq!(decoded.usage().output_tokens().value(), Some(2));
    assert_eq!(
        decoded.usage().input_tokens_ambiguous().provenance(),
        Some(&UsageProvenance::ProviderReported)
    );
}

#[test]
fn v2_neither_encodes_nor_decodes_the_legacy_input_coordinate() {
    let fact = UsageObservedFact::new(
        attempt(),
        AttemptUsage::observed(
            UsageField::reported(count(11)),
            UsageField::reported(count(8)),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::Terminal,
        ),
    );
    let encoded = encode_observed(&fact);
    let usage = match encoded.get("usage").and_then(Value::as_object) {
        Some(usage) => usage,
        None => panic!("v2 usage is an object"),
    };
    assert!(
        !usage.contains_key("legacyInputTokens"),
        "v2 encoder authored legacyInputTokens"
    );
    assert_eq!(
        decode_observed(&json!({
            "attempt": "pat_1",
            "usage": {
                "legacyInputTokens": {
                    "count": 10,
                    "provenance": "provider-reported"
                },
                "phase": "terminal"
            }
        })),
        Err(PayloadError::UnknownField {
            at: OBSERVED_V2_USAGE,
            field: "legacyInputTokens".to_owned(),
        }),
        "v2 decoder accepted legacyInputTokens"
    );
}

/// Canonical bytes are what the store hashes. Encoding the same fact twice
/// must produce the same bytes, or an exact retry would diverge instead of
/// reconciling.
#[test]
fn canonical_bytes_are_stable_across_encodings() {
    let fact = appended(
        NormalizedContentItem::new(ContentOrdinal::FIRST, ContentKind::Text, reference()),
        CaptureDisposition::Complete,
    );
    assert_eq!(
        encode_canonical_appended(&fact),
        encode_canonical_appended(&fact)
    );
    let opened = AssistantMessageOpenedFact::new(attempt(), step(), message());
    assert_eq!(
        encode_canonical_opened(&opened),
        encode_canonical_opened(&opened)
    );
    let observed = UsageObservedFact::new(
        attempt(),
        AttemptUsage::observed(
            UsageField::ABSENT,
            reported(1),
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageField::ABSENT,
            UsageObservationPhase::Terminal,
        ),
    );
    assert_eq!(
        encode_canonical_observed(&observed),
        encode_canonical_observed(&observed)
    );
}

/// THE ROW `IGSD07` NEEDED. A complete capture and a truncated one carry the
/// same length and the same digest -- a bounded producer and an unbounded one
/// are indistinguishable by reference alone. The disposition is what separates
/// them, so it round-trips exactly and a truncated capture keeps its captured
/// count.
#[test]
fn a_truncated_capture_is_distinguishable_from_a_complete_one() {
    let complete = appended(
        NormalizedContentItem::new(ContentOrdinal::FIRST, ContentKind::Text, reference()),
        CaptureDisposition::Complete,
    );
    let truncated = appended(
        NormalizedContentItem::new(ContentOrdinal::FIRST, ContentKind::Text, reference()),
        // Exactly the referenced artifact's length: the two coordinates are
        // bound to each other, so 12 here is the payload's own length and not
        // an arbitrary count.
        CaptureDisposition::TruncatedAtBoundary { captured: 12 },
    );
    // The two facts reference IDENTICAL bytes and are still not equal.
    assert_eq!(complete.item(), truncated.item());
    assert_ne!(complete, truncated);
    assert_ne!(
        encode_canonical_appended(&complete),
        encode_canonical_appended(&truncated)
    );
    for fact in [&complete, &truncated] {
        match decode_appended(&encode_appended(fact)) {
            Ok(decoded) => assert_eq!(&decoded, fact),
            Err(error) => panic!("both dispositions must decode: {error}"),
        }
    }
}

/// A truncated item without its captured count cannot say how much was kept,
/// so it is refused rather than decoded as an unquantified truncation.
#[test]
fn a_truncated_item_refuses_a_missing_captured_count() {
    let refusal = decode_appended(&json!({
        "message": "msg_df0a",
        "item": {
            "ordinal": 0,
            "kind": "text",
            "payload": {"id": "art_1", "length": 12, "digest": "sha256:aa"},
            "disposition": "truncated",
        },
    }));
    assert_eq!(
        refusal,
        Err(PayloadError::MissingField {
            at: APPENDED_ITEM,
            field: "captured".to_owned(),
        })
    );
}

/// An item with no disposition at all is refused. Defaulting it to complete
/// would report every truncated capture as the whole value.
#[test]
fn an_item_without_a_disposition_is_refused_rather_than_defaulted() {
    let refusal = decode_appended(&json!({
        "message": "msg_df0a",
        "item": {
            "ordinal": 0,
            "kind": "text",
            "payload": {"id": "art_1", "length": 12, "digest": "sha256:aa"},
        },
    }));
    assert_eq!(
        refusal,
        Err(PayloadError::MissingField {
            at: APPENDED_ITEM,
            field: "disposition".to_owned(),
        })
    );
}

/// A complete item that also reports a captured count is a contradiction.
///
/// The shared field list named `captured` for every disposition, so this
/// payload DECODED — the complete arm simply ignored the field — and then
/// re-encoded without it. A payload that does not re-encode to itself is not
/// canonical, and what it admitted was "the whole value, of which this much was
/// kept".
#[test]
fn a_complete_item_refuses_a_captured_count() {
    let refusal = decode_appended(&json!({
        "message": "msg_df0a",
        "item": {
            "ordinal": 0,
            "kind": "text",
            "payload": {"id": "art_1", "length": 12, "digest": "sha256:aa"},
            "disposition": "complete",
            "captured": 12
        }
    }));
    match refusal {
        Err(PayloadError::UnknownField { field, .. }) => assert_eq!(field, "captured"),
        other => panic!("a complete capture keeps no captured count, got {other:?}"),
    }
}

/// A truncated item whose captured count disagrees with the artifact it
/// references is refused.
///
/// `captured` is not free text: it says how many bytes of the value were kept,
/// and the bytes that were kept ARE the referenced artifact. A count that
/// disagrees describes an artifact that does not exist.
#[test]
fn a_truncated_item_refuses_a_captured_count_its_payload_contradicts() {
    let refusal = decode_appended(&json!({
        "message": "msg_df0a",
        "item": {
            "ordinal": 0,
            "kind": "text",
            "payload": {"id": "art_1", "length": 12, "digest": "sha256:aa"},
            "disposition": "truncated",
            "captured": 5
        }
    }));
    match refusal {
        Err(PayloadError::InvalidField { field, .. }) => assert_eq!(field, "captured"),
        other => panic!("captured must be the payload's own length, got {other:?}"),
    }
}

/// The PRODUCER door, closed with the same predicate as the decoder.
///
/// Before this the constructor was infallible, so the decoder-side rule above
/// was half a rule: the contradiction was unreachable through stored bytes and
/// wide open through the public type, and a producer could hand the encoder a
/// fact whose own canonical bytes would not decode.
#[test]
fn the_public_constructor_refuses_a_disposition_its_payload_contradicts() {
    match ContentAppendedFact::new(
        message(),
        NormalizedContentItem::new(ContentOrdinal::FIRST, ContentKind::Text, reference()),
        CaptureDisposition::TruncatedAtBoundary { captured: 5 },
    ) {
        Err(PayloadError::InvalidField { field, .. }) => assert_eq!(field, "captured"),
        other => panic!("the constructor must refuse the contradiction, got {other:?}"),
    }
    // And the agreeing pair is still constructible, so the guard refuses the
    // CONTRADICTION rather than refusing truncation.
    match ContentAppendedFact::new(
        message(),
        NormalizedContentItem::new(ContentOrdinal::FIRST, ContentKind::Text, reference()),
        CaptureDisposition::TruncatedAtBoundary { captured: 12 },
    ) {
        Ok(fact) => assert_eq!(
            fact.disposition(),
            CaptureDisposition::TruncatedAtBoundary { captured: 12 }
        ),
        Err(error) => panic!("an agreeing truncation must construct: {error}"),
    }
}
