use super::*;

fn native_input() -> Result<AttemptInput, Box<dyn std::error::Error>> {
    let mut input = reasoning_input()?;
    let item = crate::native_search::SearchHistoryItem::from_output(&serde_json::json!({
        "type":"web_search_call","id":"ws_replay","status":"completed",
        "action":{"type":"search","queries":["a recorded question"],"sources":[{"type":"url","url":"https://example.com/source?case=Kept#section"}]}
    }))?;
    let state = search_state(
        &item,
        &ReplayBinding::for_search(&input).ok_or("search binding")?,
    )?;
    let model = input.request.model();
    let replay = ProviderReplay::restore(
        model.provider().clone(),
        model.model().clone(),
        model.variant().cloned(),
        std::str::from_utf8(state.capture_bytes().ok_or("search state")?)?,
    )?;
    let ProviderRequest::OpenAiResponses(request) = &mut input.request else {
        return Err("family".into());
    };
    request.input = vec![CodexInputItem::ProviderReplay(replay)];
    Ok(input)
}

#[test]
fn native_search_replay_keeps_its_binding_and_retains_data_after_every_switch(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut kept = native_input()?;
    drop_unreplayable_reasoning(&mut kept);
    assert_eq!(replays(&kept), 1);
    assert!(adapter(1).stage(native_input()?).is_ok());
    for changed in [
        "model",
        "endpoint",
        "codex",
        "account",
        "anthropic",
        "compatible",
    ] {
        let mut switched = switched_from(native_input()?, changed)?;
        drop_unreplayable_reasoning(&mut switched);
        assert_eq!(replays(&switched), 0, "{changed}");
        let items = match &switched.request {
            ProviderRequest::OpenAiResponses(request) => &request.input,
            ProviderRequest::ChatGptCodex(request) => &request.input,
            ProviderRequest::AnthropicMessages(request)
            | ProviderRequest::VertexAnthropic(request) => &request.messages,
            ProviderRequest::OpenAiCompatible(request) | ProviderRequest::WatsonxChat(request) => {
                &request.messages
            }
            ProviderRequest::BedrockConverse(request) => &request.messages,
            ProviderRequest::GoogleGemini(request) | ProviderRequest::VertexGemini(request) => {
                &request.messages
            }
            ProviderRequest::CohereChat(request) => &request.messages,
        };
        assert!(items.iter().any(|item|matches!(item,CodexInputItem::Message(message)
            if message.role==crate::MessageRole::Assistant && message.content.contains("a recorded question")
                && message.content.contains("example.com/source?case=Kept#section"))),"{changed} lost recorded search data");
        assert!(!items.iter().any(|item| matches!(
            item,
            CodexInputItem::ToolCall { .. } | CodexInputItem::ToolOutput { .. }
        )));
        assert!(
            adapter(1)
                .stage(switched_from(native_input()?, changed)?)
                .is_ok(),
            "{changed} refused instead of preserving data"
        );
    }
    Ok(())
}
