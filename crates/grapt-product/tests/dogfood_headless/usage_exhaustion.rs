use super::*;

const EXHAUSTED: &str = r#"{"error":{"type":"usage_limit_reached","resets_at":1893456000,"message":"private-provider-account"}}"#;

fn assert_recovery(message: &str, reset_known: bool) {
    let lower = message.to_ascii_lowercase();
    assert!(
        lower.contains("usage") && lower.contains("limit"),
        "{message}"
    );
    assert!(
        lower.contains("switch") && lower.contains("account"),
        "{message}"
    );
    let shows_reset = message.contains("1893456000") || message.contains("2030-01-01");
    assert_eq!(
        shows_reset, reset_known,
        "reset evidence changed: {message}"
    );
    assert!(!message.contains("private-"), "{message}");
}

#[test]
fn usage_exhaustion_publishes_safe_recovery_without_spending_the_retry_budget() {
    let _journey = process_journey_custody();
    for (label, body, reset_known) in [
        ("usage-reset", EXHAUSTED, true),
        (
            "usage-no-reset",
            r#"{"error":{"type":"usage_limit_reached","message":"private-provider-account"}}"#,
            false,
        ),
        (
            "usage-invalid-reset",
            r#"{"error":{"type":"usage_limit_reached","resets_at":"private-reset","message":"private-provider-account"}}"#,
            false,
        ),
    ] {
        let root = root(label);
        prepare(&root, 120_000);
        let provider = ProviderFixture::start(vec![Script::Refused { status: 429, body }; 5]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let session = create(address);
        let (events, connected) = event_stream(address);
        events
            .set_read_timeout(Some(Duration::from_secs(45)))
            .unwrap_or_else(|error| panic!("exhaustion event bound: {error}"));
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"continue the task"}"#,
        );
        let observed = collect(events, connected, "execution.failed");
        permission_residuals::wait_session_idle(address, &session);
        child.stop_after_witness(&root);
        assert_recovery(&observed, reset_known);
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "exhaustion spent the retry budget"
        );
        drop(child);
        process_fixture::cleanup(&root);
    }
}

#[test]
fn ordinary_rate_limiting_still_recovers_in_the_same_execution() {
    let _journey = process_journey_custody();
    let root = root("usage-rate-limit");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(vec![
        Script::Refused {
            status: 429,
            body: r#"{"error":{"message":"usage_limit_reached private-provider-account","resets_at":1893456000}}"#,
        },
        Script::SuccessText("rate limit recovered"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (events, connected) = event_stream(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"continue after throttling"}"#,
    );
    let observed = collect(events, connected, "execution.succeeded");
    child.stop_after_witness(&root);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert!(observed.contains("rate limit recovered"), "{observed}");
    assert!(
        !observed.contains("private-") && !observed.contains("1893456000"),
        "{observed}"
    );
    drop(child);
    process_fixture::cleanup(&root);
}

#[test]
fn exhausted_compaction_is_non_transient_and_keeps_its_reset_guidance() {
    let _journey = process_journey_custody();
    let failure = Script::Refused {
        status: 429,
        body: EXHAUSTED,
    };
    let journey = Journey::failed("usage-compaction", failure.clone(), vec![failure; 4], false);
    let history = json(
        journey.address,
        "GET",
        &format!("/api/session/{}/message", journey.session),
        "",
    );
    assert_recovery(&history.to_string(), true);
    journey.assert_cause("usage");
    journey.finish(2, 1);
}
