//! Recorded Responses failure shape, plus incomplete and successful variants.
use super::*;

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn terminal_usage_defects_preserve_backend_failures_and_successful_answers() {
    let cases: serde_json::Value =
        serde_json::from_str(include_str!("responses-terminal-usage.json"))
            .unwrap_or_else(|error| panic!("terminal fixtures: {error}"));
    #[derive(Debug)]
    struct Clock;
    impl grapt_types::ClockAuthority for Clock {
        fn now(&self) -> grapt_types::Timestamp {
            grapt_types::Timestamp::from_millis(0)
        }
    }
    for family in [
        ProtocolFamily::OpenAiResponses,
        ProtocolFamily::ChatGptCodex,
    ] {
        for case in cases.as_array().unwrap_or_else(|| panic!("fixture array")) {
            let terminal = case.to_string();
            let status = if family == ProtocolFamily::ChatGptCodex {
                201
            } else {
                200
            };
            let fixture = serve(status, &[], vec![sse(&[
                r#"{"type":"response.output_text.delta","item_id":"answer","delta":"kept answer"}"#,
                &terminal,
            ])]).await;
            let request = match family {
                ProtocolFamily::ChatGptCodex => {
                    crate::ProviderRequest::ChatGptCodex(crate::ChatGptCodexRequest {
                        model: responses_request().model().clone(),
                        input: vec![user("Hello").into()],
                        prompt_cache_key: None,
                        tools: Vec::new(),
                        web_search: None,
                        tool_choice: None,
                        reasoning: Default::default(),
                        turn: Default::default(),
                    })
                }
                _ => responses_request(),
            };
            let (observer, samples) = crate::DispatchObserver::bounded(Arc::new(Clock));
            let adapter = adapter(1).with_dispatch(observer);
            let open = adapter
                .open(input("att_terminal_usage", &fixture.base, family, request))
                .await
                .unwrap_or_else(|error| panic!("fixture open: {error}"));
            let mut stream = match open {
                ProviderOpen::Streaming(stream) => stream,
                _ => panic!("expected streaming response"),
            };
            let mut fragments = Vec::new();
            let mut detail = None;
            while let Some(fragment) = stream.next_fragment().await {
                if let Some(observed) = stream.take_error_detail() {
                    detail = Some(observed);
                }
                fragments.push(fragment);
            }
            let sample = samples
                .try_next()
                .unwrap_or_else(|error| panic!("dispatch: {error}"));
            assert_eq!(
                sample.usage_warning.is_some(),
                case["type"] == "response.completed"
            );
            assert!(sample.usage.is_empty());
            assert!(!format!("{fragments:?}").contains("token-value-never-printed"));
            assert!(!format!("{fragments:?}").contains("sk-private-echo"));
            assert_eq!(text_of(&fragments), "kept answer");
            assert!(!fragments
                .iter()
                .any(|fragment| matches!(fragment, AttemptFragment::Usage { .. })));
            if case["type"] == "response.completed" {
                assert_eq!(
                    failure_of(&fragments),
                    None,
                    "good answer must survive bad accounting"
                );
                assert!(fragments.iter().any(|fragment| matches!(
                    fragment,
                    AttemptFragment::ProviderTerminal { usage: None, .. }
                )));
            } else {
                let (class, message) = fragments
                    .iter()
                    .find_map(|fragment| match fragment {
                        AttemptFragment::ProviderFailure { class, message, .. } => {
                            Some((class, message))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("backend failure missing"));
                assert_eq!(
                    *class,
                    ProviderFailureClass::InvalidRequest,
                    "usage cannot mask the backend cause"
                );
                assert!(message.contains("context_length_exceeded"));
                let detail = detail.unwrap_or_else(|| panic!("terminal diagnosis missing"));
                assert_eq!(detail.status(), status);
                if case["type"] == "response.failed" {
                    assert!(message.contains(r"C:\backend\reply"));
                }
                let frame = detail
                    .terminal_frame()
                    .unwrap_or_else(|| panic!("terminal frame missing"));
                assert!(frame.as_str().len() <= crate::TERMINAL_DETAIL_BYTES);
                assert!(serde_json::from_str::<serde_json::Value>(frame.as_str()).is_ok());
                for secret in [
                    "token-value-never-printed",
                    "sk-private-echo",
                    "PRIVATE_OUTPUT_CANARY",
                    "PRIVATE_METADATA_CANARY",
                ] {
                    assert!(!frame.as_str().contains(secret));
                    assert!(!format!("{detail:?}").contains(secret));
                }
            }
        }
    }
}
