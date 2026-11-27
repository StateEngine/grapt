use super::*;

// StateEngine's independently pinned provider fixture at fe72f04a6ba3422a1e6cd8131e0133d7e8b17d46:
// test/fixtures/remote-seat-mail-row-refusals-v2.json.
const PROVIDER_REASONS: &[&str] = &[
    "malformed_json",
    "line_too_large",
    "duplicate_msg_id",
    "ack_ambiguous_duplicate_msg_id",
    "MalformedEnvelope",
    "UnknownKind",
    "UnknownArchiveReason",
    "MessageBodyExceeded",
    "PageBodyBytesExceeded",
    "PageDecodedBytesExceeded",
    "ReferenceCountExceeded",
    "ReferenceBytesExceeded",
    "ProjectProvenanceMissing",
    "SupersessionConflict",
];

#[test]
fn provider_v2_refusals_preserve_unrelated_healthy_mail_and_progress() -> TestResult {
    for reason in PROVIDER_REASONS {
        let mut value = mixed();
        value["refused_rows"][0]["msg_id"] = json!("refused-family");
        value["refused_rows"][0]["reason"] = json!(reason);
        let page = parsed(&value, 64)?;
        assert_eq!(page.messages().len(), 1);
        assert_eq!(page.messages()[0].source_message().as_str(), "one");
        assert_eq!(page.refused_rows().len(), 1);
        assert_eq!(page.refused_rows()[0].reason().as_str(), *reason);
        assert_eq!(page.entry_count(), 2);
        assert_eq!(page.last_cursor().map(|c| c.as_str()), Some("ref1.opaque"));
    }
    Ok(())
}

#[test]
fn ambiguous_families_cannot_also_appear_as_healthy_or_unknown_kind_rows() -> TestResult {
    for reason in ["duplicate_msg_id", "ack_ambiguous_duplicate_msg_id"] {
        for kind in ["handoff", "future-kind"] {
            let mut value = mixed();
            value["messages"][0]["kind"] = json!(kind);
            value["refused_rows"][0]["msg_id"] = json!("one");
            value["refused_rows"][0]["reason"] = json!(reason);
            let error = parsed(&value, 64)
                .err()
                .ok_or("ambiguous family remained in the healthy page")?;
            let failure = error
                .downcast_ref::<OperationFailure>()
                .ok_or("wrong error layer")?;
            assert!(matches!(
                failure.cause,
                OperationCause::Connection(ConnectionFailure::InvalidEnvelope)
            ));
            assert_eq!(failure.effect, WriteEffect::None);
        }
    }
    Ok(())
}

#[test]
fn refusal_vocabulary_keeps_fixed_legacy_schema_codes_without_accepting_new_details() -> TestResult
{
    for reason in [
        "schema:record_object",
        "schema:message.body",
        "schema:native-mail-delivery.seat",
    ] {
        let mut value = mixed();
        value["refused_rows"][0]["reason"] = json!(reason);
        assert_eq!(
            parsed(&value, 64)?.refused_rows()[0].reason().as_str(),
            reason
        );
    }
    for reason in [
        "schema:future-field",
        "duplicate_msg_id_future",
        "future-refusal",
    ] {
        let mut value = mixed();
        value["refused_rows"][0]["reason"] = json!(reason);
        assert!(
            parsed(&value, 64).is_err(),
            "accepted unknown reason {reason}"
        );
    }
    Ok(())
}
