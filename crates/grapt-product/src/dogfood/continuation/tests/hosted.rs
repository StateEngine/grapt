use super::*;

#[test]
fn reopened_hosted_result_is_retained_as_data_and_never_resurrected_as_a_function(
) -> Result<(), String> {
    let mut fixture = Fixture::new("hosted-execution-ownership")?;
    fixture.record_owned_exchange_call(
        (true, false, false),
        "web_search",
        "web_search",
        b"a recorded provider-hosted result",
        r#"{"query":"fixture"}"#,
        ToolExecution::ProviderHosted,
    )?;
    let current = fixture.input.clone();
    fixture.close();
    let input = fixture
        .reopen(current)?
        .request_input("use the tool", true)?;
    assert!(!input.iter().any(|item| matches!(
        item,
        CodexInputItem::ToolCall { .. } | CodexInputItem::ToolOutput { .. }
    )));
    assert!(input.iter().any(|item| matches!(item, CodexInputItem::Message(message)
        if message.role == MessageRole::Assistant && message.content.contains("Provider-hosted tool web_search")
            && message.content.contains("a recorded provider-hosted result"))));
    Ok(())
}

#[test]
fn reopening_native_search_keeps_one_native_item_and_no_function_exchange(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new("native-search-reopen")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "use the tool",
    )?;
    let call = ToolCallId::new("ws_reopen").ok_or("call")?;
    let native = serde_json::json!({"type":"web_search_call","id":call.as_str(),"status":"completed",
        "action":{"type":"search","queries":["recorded question"],"sources":[{"type":"url","url":"https://example.com/source"}]}});
    let state = AdapterState::retain(
        serde_json::json!({"kind":"openai-web-search-v1","item":native,"binding":vec![0u8;32]})
            .to_string(),
    )?;
    let selection = grapt_event::session_message::AssistantSelection::new(
        AgentId::new("agt_native_fixture").ok_or("agent")?,
        grapt_types::ProviderId::new("openai").ok_or("provider")?,
        grapt_types::ModelId::new("native-fixture").ok_or("model")?,
        None,
    );
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds::default()),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(1).ok_or("step")?,
        None,
    )?
    .with_selection(selection)?;
    owner.commit(DurableFact::AttemptStarted {
        attempt: PhysicalAttemptId::new("native-search-reopen").ok_or("attempt")?,
    })?;
    owner.commit(DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::ToolCalled {
            call: call.clone(),
            tool: "web_search".into(),
            input: r#"{"type":"search","queries":["recorded question"]}"#.into(),
            execution: ToolExecution::ProviderHosted,
            state: AdapterState::none(),
        }),
    })?;
    let output =
        owner.capture_hosted_output("a recorded result with https://example.com/source")?;
    owner.commit(DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::HostedToolResult {
            call: call.clone(),
            tool: "web_search".into(),
            result: "a recorded result with https://example.com/source".into(),
            state,
        }),
    })?;
    owner.commit(DurableFact::ToolTerminal {
        call,
        outcome: ToolTerminalOutcome::Completed {
            output: Some(output),
        },
    })?;
    owner.commit(DurableFact::StepEnded {
        finish: FinishReason::Stop,
    })?;
    drop(owner);
    let next = "msg_after_native_search".parse::<MessageId>()?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &next,
        "a later question",
    )?;
    fixture.close();
    let input = fixture
        .reopen(next)?
        .request_input("a later question", false)?;
    assert_eq!(input.iter().filter(|item|matches!(item,CodexInputItem::ProviderReplay(replay) if replay.native_search_call().is_some())).count(),1);
    assert!(!input.iter().any(|item| matches!(
        item,
        CodexInputItem::ToolCall { .. } | CodexInputItem::ToolOutput { .. }
    )));
    assert!(!input.iter().any(|item|matches!(item,CodexInputItem::Message(message) if message.content.contains("Provider-hosted tool web_search"))));
    assert!(
        matches!(input.last(),Some(CodexInputItem::Message(message)) if message.role==MessageRole::User && message.content=="a later question")
    );
    Ok(())
}
