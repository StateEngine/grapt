//! A provider that refuses a request leaves a diagnostic line naming its HTTP
//! status and its own machine words, and never its message.
use super::*;
use std::time::Instant;

/// This fixture's wall-clock deadline.
#[allow(clippy::disallowed_methods)]
fn now() -> Instant {
    Instant::now()
}

#[test]
fn a_refused_request_is_recorded_without_the_providers_message() {
    let _journey = process_journey_custody();
    let root = root("provider-refusal");
    prepare(&root, 200_000);
    // The code and the message both carry a secret, as a vendor error can
    // echo request content: only a known word may be written as itself.
    let provider = ProviderFixture::start(vec![Script::Refused {
        status: 400,
        body: r#"{"error":{"type":"invalid_request_error","code":"sk-secret-xyz","message":"key sk-secret-xyz is bad"}}"#,
    }]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let body = serde_json::json!({"id": "msg_provider_refusal", "text": "Say hello.", "delivery": "queue", "resume": true});
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &body.to_string(),
    );
    permission_residuals::wait_session_idle(address, &session);
    // The line is written off the execution path; wait for it, within a bound.
    let log = root.join("diagnostics/provider-errors.jsonl");
    let by = now() + Duration::from_secs(10);
    let text = loop {
        let text = fs::read_to_string(&log).unwrap_or_default();
        if text.ends_with('\n') || now() > by {
            break text;
        }
        thread::sleep(Duration::from_millis(20));
    };
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);

    assert!(!text.contains("sk-secret"), "{text}");
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("bad diagnostic line {error}: {line}"))
        })
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    let line = &lines[0];
    assert_eq!(line["schema"], "grapt.provider-refusal.v1", "{text}");
    assert_eq!(line["status"], 400, "{text}");
    assert_eq!(line["error_type"], "invalid_request_error", "{text}");
    assert_eq!(line["error_code"], "unlisted", "{text}");
    assert!(line["attempt"]
        .as_str()
        .is_some_and(|attempt| !attempt.is_empty()));
    assert!(line.get("message").is_none(), "{text}");
}
