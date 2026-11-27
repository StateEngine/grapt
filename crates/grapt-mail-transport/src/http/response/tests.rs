use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn refused_wire_shape_survives_redaction_including_duplicate_keys() {
    let raw = br#"{"ok":false,"failure":{"family":"TransportFailure","tag":"Disconnected","reason":"echo \u0063ap-private and cursor-private","cursor":"new-private-cursor"}}"#;
    let evidence = Evidence::capture(
        "subscribe_and_snapshot",
        503,
        "application/json",
        raw,
        "cap-private",
        &json!({"binding_id":"binding-private","resume":{"from_checkpoint":"cursor-private"}}),
    );
    let text = evidence.render("envelope");
    for expected in [
        "subscribe_and_snapshot",
        "503",
        "application/json",
        "TransportFailure",
        "Disconnected",
    ] {
        assert!(
            text.contains(expected),
            "diagnostic lost {expected}: {text}"
        );
    }
    let duplicate = Evidence::capture(
        "open",
        400,
        "application/json",
        br#"{"ok":false,"ok":true,"failure":{}}"#,
        "cap-private",
        &json!({}),
    )
    .render("envelope");
    assert!(duplicate.contains("invalid or duplicate JSON"));
    for private in [
        "cap-private",
        "cursor-private",
        "binding-private",
        "new-private-cursor",
    ] {
        assert!(
            !text.contains(private),
            "private value reached diagnostics: {private}"
        );
    }
}

#[test]
fn incomplete_strings_and_nested_non_string_failure_fields_cannot_reveal_handles() {
    for raw in [
        br#"{"failure":{"reason":["fresh-private-handle"]}}"#.as_slice(),
        br#"{"failure":{"reason":"cap-priv"#.as_slice(),
    ] {
        let evidence = Evidence::capture(
            "open",
            400,
            "application/json",
            raw,
            "cap-private",
            &json!({}),
        );
        let text = evidence.render("envelope");
        assert!(!text.contains("fresh-private-handle"));
        assert!(!text.contains("cap-priv"));
    }
}

#[test]
fn diagnostics_are_bounded_and_cannot_repaint_the_terminal_or_expose_a_cut_secret() {
    let body = format!("{}cap-private\u{1b}[31m\u{202e}", "界".repeat(3000));
    let evidence = Evidence::capture(
        "next_change",
        500,
        "application/json",
        body.as_bytes(),
        "cap-private",
        &json!({}),
    );
    let text = evidence.render("envelope");
    assert!(text.len() < 8500);
    assert!(text.contains("diagnostic bound"));
    assert!(!text.contains("cap-private"));
    assert!(!text.chars().any(char::is_control));
    assert!(!text.contains('\u{202e}'));
}

#[test]
fn exceeding_private_value_bounds_suppresses_the_response() {
    let args = json!((0..300).map(|i| format!("private-{i}")).collect::<Vec<_>>());
    let evidence = Evidence::capture(
        "open",
        400,
        "cap-private",
        b"unexpected private-299",
        "cap-private",
        &args,
    );
    let text = evidence.render("envelope");
    assert!(text.contains("private-value bound exceeded"));
    assert!(!text.contains("private-299"));
    assert!(!text.contains("cap-private"));
}

#[test]
fn value_decoder_keeps_refusal_effects_and_never_promotes_malformed_success() -> TestResult {
    let response = Response {
        value: json!({"generation":"not-an-integer","token":"fresh-private"}),
        evidence: Evidence::capture(
            "next_change",
            200,
            "application/json",
            br#"{"ok":true,"value":{"generation":"not-an-integer","token":"fresh-private"}}"#,
            "cap-private",
            &json!({"subscription_id":"subscription-private"}),
        ),
    };
    let failure = response
        .decode(|value| decode::integer(value, "generation"))
        .err()
        .ok_or("malformed generation was accepted")?;
    assert_eq!(
        failure.cause,
        OperationCause::Connection(ConnectionFailure::InvalidEnvelope)
    );
    assert_eq!(failure.effect, WriteEffect::None);
    Ok(())
}

#[test]
fn response_owned_text_keys_and_header_parameters_stay_private() {
    for raw in [
        b"response-owned-secret".as_slice(),
        br#"{"response-owned-secret":true}"#.as_slice(),
        br#"{"failure":{"family":"TransportFailure","reason":"response-owned-secret","detail":"response-owned-secret"}}"#.as_slice(),
    ] {
        let text = Evidence::capture("open", 400,
            "application/json", raw, "cap-private", &json!({}))
            .render("envelope");
        assert!(!text.contains("response-owned-secret"), "response-owned data reached diagnostics: {text}");
    }
    let header = Evidence::capture(
        "open",
        400,
        "application/json; response-owned-secret",
        b"{}",
        "cap-private",
        &json!({}),
    )
    .render("headers");
    assert!(!header.contains("response-owned-secret"));
    let numeric = Evidence::capture(
        "open",
        400,
        "application/json",
        b"123456789",
        "cap-private",
        &json!({}),
    )
    .render("envelope");
    assert!(!numeric.contains("123456789"));
}
