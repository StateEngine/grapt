use super::*;

fn artifact(length: u64, disposition: CaptureDisposition) -> ToolTerminalArtifact {
    let id = ArtifactId::new("art_tool_terminal").unwrap_or_else(|| panic!("valid id"));
    let digest =
        ArtifactDigest::new("sha256-tool-terminal").unwrap_or_else(|| panic!("valid digest"));
    ToolTerminalArtifact::new(ArtifactRef::new(id, length, digest), disposition)
        .unwrap_or_else(|| panic!("coherent artifact pair"))
}

fn fact(outcome: ToolTerminalOutcome) -> ToolTerminalFact {
    let attempt =
        PhysicalAttemptId::new("att_tool_terminal").unwrap_or_else(|| panic!("valid attempt"));
    let call = ToolCallId::new("call_tool_terminal").unwrap_or_else(|| panic!("valid call"));
    ToolTerminalFact::new(attempt, call, outcome)
}

#[test]
fn every_closed_terminal_round_trips_canonically() {
    let outcomes = [
        ToolTerminalOutcome::Completed { output: None },
        ToolTerminalOutcome::Completed {
            output: Some(artifact(0, CaptureDisposition::Complete)),
        },
        ToolTerminalOutcome::Failed {
            reason: ToolFailureReason::EffectFailed,
            detail: Some(artifact(
                3,
                CaptureDisposition::TruncatedAtBoundary { captured: 3 },
            )),
        },
        ToolTerminalOutcome::Cancelled {
            by: InterruptionSource::ManagedShutdown,
        },
        ToolTerminalOutcome::Failed {
            reason: ToolFailureReason::EffectUnknown,
            detail: None,
        },
        ToolTerminalOutcome::Failed {
            reason: ToolFailureReason::ApprovalUnavailable,
            detail: Some(artifact(12, CaptureDisposition::Complete)),
        },
        ToolTerminalOutcome::ResultMissing,
    ];
    for outcome in outcomes {
        let expected = fact(outcome);
        let decoded = decode(&encode(&expected)).unwrap_or_else(|error| panic!("decode: {error}"));
        assert_eq!(decoded, expected);
        assert_eq!(
            encode_canonical_bytes(&decoded),
            encode_canonical_bytes(&expected)
        );
    }
}

#[test]
fn half_pairs_nulls_unknown_fields_and_variants_refuse() {
    let mut value = encode(&fact(ToolTerminalOutcome::Completed {
        output: Some(artifact(3, CaptureDisposition::Complete)),
    }));
    let output = value
        .get_mut("outcome")
        .and_then(Value::as_object_mut)
        .and_then(|outcome| outcome.get_mut("output"))
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("encoded output object"));
    output.remove("digest");
    assert!(matches!(
        decode(&value),
        Err(PayloadError::MissingField { .. })
    ));

    let mut null = encode(&fact(ToolTerminalOutcome::Completed { output: None }));
    null.as_object_mut()
        .unwrap_or_else(|| panic!("root object"))
        .insert("call".to_owned(), Value::Null);
    assert!(matches!(decode(&null), Err(PayloadError::NullField { .. })));

    let mut unknown = encode(&fact(ToolTerminalOutcome::ResultMissing));
    unknown
        .as_object_mut()
        .unwrap_or_else(|| panic!("root object"))
        .insert("raw_output".to_owned(), Value::from("forbidden"));
    assert!(matches!(
        decode(&unknown),
        Err(PayloadError::UnknownField { .. })
    ));

    let mut variant = encode(&fact(ToolTerminalOutcome::ResultMissing));
    variant
        .get_mut("outcome")
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("outcome object"))
        .insert("kind".to_owned(), Value::from("other"));
    assert!(matches!(
        decode(&variant),
        Err(PayloadError::UnknownVariant { .. })
    ));
}

#[test]
fn truncated_capture_must_equal_the_reference_length() {
    let mut value = encode(&fact(ToolTerminalOutcome::Completed {
        output: Some(artifact(
            3,
            CaptureDisposition::TruncatedAtBoundary { captured: 3 },
        )),
    }));
    value
        .get_mut("outcome")
        .and_then(Value::as_object_mut)
        .and_then(|outcome| outcome.get_mut("output"))
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("output object"))
        .insert("captured".to_owned(), Value::from(2));
    assert!(matches!(
        decode(&value),
        Err(PayloadError::InvalidField { .. })
    ));
}
