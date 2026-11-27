//! Codex sticky routing (x-codex-turn-state) on the wire.
use super::*;

fn codex_with(turn: &crate::CodexTurn) -> crate::request::ProviderRequest {
    crate::request::ProviderRequest::ChatGptCodex(crate::ChatGptCodexRequest {
        model: responses_request().model().clone(),
        input: vec![user("Hello").into()],
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: turn.clone(),
    })
}

/// Codex's sticky routing (x-codex-turn-state): the first response of an
/// execution carries the token and every later request of that execution
/// sends it back; another execution, another account and another route
/// never send it.
#[tokio::test]
async fn the_turn_state_token_is_echoed_within_one_execution_only() {
    let completed = sse(&[
        r#"{"type":"response.completed","response":{"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}"#,
    ]);
    let fixture = serve(
        200,
        &[("x-codex-turn-state", "TS_STICKY_TOKEN")],
        vec![completed],
    )
    .await;
    let sent = |fixture: &crate::transport::fixtures::Fixture| {
        fixture
            .head
            .lock()
            .map(|head| head.contains("x-codex-turn-state: ts_sticky_token"))
            .unwrap_or(false)
    };
    #[derive(Debug)]
    struct Clock;
    impl grapt_types::ClockAuthority for Clock {
        fn now(&self) -> grapt_types::Timestamp {
            grapt_types::Timestamp::from_millis(0)
        }
    }
    let (observer, samples) = crate::DispatchObserver::bounded(std::sync::Arc::new(Clock));
    let adapter = adapter(4).with_dispatch(observer);
    let execution = crate::CodexTurn::default();
    let open = |id: &str, request| {
        adapter.open(input(
            id,
            &fixture.base,
            ProtocolFamily::ChatGptCodex,
            request,
        ))
    };
    match open("att_turn_1", codex_with(&execution)).await {
        Ok(open) => drop(drain(open).await),
        Err(error) => panic!("open: {error}"),
    }
    assert!(
        !sent(&fixture),
        "the first request of an execution has no token"
    );
    let first = samples
        .try_next()
        .unwrap_or_else(|error| panic!("first diagnostic: {error}"));
    match open("att_turn_2", codex_with(&execution)).await {
        Ok(open) => drop(drain(open).await),
        Err(error) => panic!("open: {error}"),
    }
    assert!(
        sent(&fixture),
        "a later request of the execution sends it back"
    );
    let second = samples
        .try_next()
        .unwrap_or_else(|error| panic!("second diagnostic: {error}"));
    let (Some(first), Some(second)) = (first.request, second.request) else {
        panic!("actual request fingerprints missing");
    };
    assert_eq!(second.compare(&first), crate::RequestDelta::RoutingChanged);
    match open("att_turn_3", codex_with(&crate::CodexTurn::default())).await {
        Ok(open) => drop(drain(open).await),
        Err(error) => panic!("open: {error}"),
    }
    assert!(!sent(&fixture), "another execution starts without it");
    let mut other = input(
        "att_turn_4",
        &fixture.base,
        ProtocolFamily::ChatGptCodex,
        codex_with(&execution),
    );
    other.lease = match crate::CredentialLease::issue(
        grapt_types::AccountId::new("other-account").unwrap_or_else(|| panic!("account")),
        other.lease.credential_version().clone(),
        crate::CredentialPlacement::BearerAuthorization,
        grapt_types::Secret::new("different-secret-never-hashed".to_owned()),
    ) {
        Ok(lease) => lease,
        Err(error) => panic!("lease: {error}"),
    };
    match adapter.open(other).await {
        Ok(open) => drop(drain(open).await),
        Err(error) => panic!("open: {error}"),
    }
    assert!(!sent(&fixture), "another account never receives the token");
    match adapter
        .open(input(
            "att_turn_5",
            &fixture.base,
            ProtocolFamily::OpenAiResponses,
            responses_request(),
        ))
        .await
    {
        Ok(open) => drop(drain(open).await),
        Err(error) => panic!("open: {error}"),
    }
    assert!(!sent(&fixture), "the public Responses route never sends it");
}
