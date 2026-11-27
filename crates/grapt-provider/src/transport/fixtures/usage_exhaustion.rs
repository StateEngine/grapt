use super::*;

async fn refusal(body: serde_json::Value) -> (AttemptFragment, crate::DispatchRefusal) {
    #[derive(Debug)]
    struct Clock;
    impl grapt_types::ClockAuthority for Clock {
        fn now(&self) -> grapt_types::Timestamp {
            grapt_types::Timestamp::from_millis(0)
        }
    }
    let fixture = serve(429, &[], vec![body.to_string().into_bytes()]).await;
    let (observer, samples) = crate::DispatchObserver::bounded(Arc::new(Clock));
    let adapter = adapter(1).with_dispatch(observer);
    let opened = adapter
        .open(input(
            "att_usage_refusal",
            &fixture.base,
            ProtocolFamily::OpenAiResponses,
            responses_request(),
        ))
        .await
        .unwrap_or_else(|error| panic!("fixture attempt refused: {error}"));
    let fragment = drain(opened)
        .await
        .into_iter()
        .find(|fragment| matches!(fragment, AttemptFragment::ProviderFailure { .. }))
        .unwrap_or_else(|| panic!("429 omitted its failure"));
    let refusal = samples
        .try_next_refusal()
        .unwrap_or_else(|error| panic!("429 omitted its dispatch refusal: {error}"));
    (fragment, refusal)
}

/// The failure's typed fields, without the message that quotes the body.
fn typed(
    failure: &AttemptFragment,
) -> Option<(
    &crate::ProviderFailureClass,
    &Option<crate::FailureClassification>,
)> {
    match failure {
        AttemptFragment::ProviderFailure {
            class,
            classification,
            ..
        } => Some((class, classification)),
        _ => None,
    }
}

#[tokio::test]
async fn explicit_usage_exhaustion_is_terminal_even_without_a_valid_reset() {
    for reset in [
        None,
        Some(serde_json::json!(1_893_456_000)),
        Some(serde_json::json!("sk-private-reset")),
        Some(serde_json::json!(-1)),
        Some(serde_json::json!(1.5)),
    ] {
        let mut error = serde_json::json!({
            "type": "usage_limit_reached", "message": "sk-private-provider-account"
        });
        if let Some(reset) = reset {
            error["resets_at"] = reset;
        }
        let (failure, refusal) = refusal(serde_json::json!({"error":error})).await;
        assert!(
            !failure.may_retry(),
            "account exhaustion was retryable: {failure:?}"
        );
        assert_eq!(
            refusal.error_type.as_ref().map(crate::VendorCode::as_str),
            Some("usage_limit_reached")
        );
        // The message quotes the body, masked: the secret-shaped values never show.
        let diagnostic = format!("{failure:?} {refusal:?}");
        assert!(!diagnostic.contains("private-"), "{diagnostic}");
    }
}

#[tokio::test]
async fn ordinary_429_and_provider_prose_do_not_prove_account_exhaustion() {
    for error in [
        serde_json::json!({"type":"rate_limit_exceeded", "resets_at":1_893_456_000}),
        serde_json::json!({"message":"usage_limit_reached sk-private-provider-account", "resets_at":1_893_456_000}),
        serde_json::json!({"type":"usage_limit_reached sk-private-provider-account"}),
        serde_json::json!({}),
    ] {
        let (failure, refusal) = refusal(serde_json::json!({"error":error})).await;
        assert!(
            failure.may_retry(),
            "ordinary rate limiting became terminal: {failure:?}"
        );
        let quoted = format!("{failure:?} {refusal:?}");
        assert!(!quoted.contains("private-"), "{quoted}");
        // The masked body may quote the reset; no typed field may carry it.
        let diagnostic = format!("{:?} {refusal:?}", typed(&failure));
        assert!(
            !diagnostic.contains("1893456000"),
            "unproven reset escaped: {diagnostic}"
        );
    }
}
