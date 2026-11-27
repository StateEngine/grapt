use super::{body, model, tool};
use crate::{
    ChatGptCodexRequest, DomainFilter, OpenAiResponsesRequest, ProviderRequest, WebSearchAccess,
    WebSearchContext, WebSearchSpec,
};
use serde_json::json;

#[test]
fn hosted_search_is_separate_from_functions_and_preserves_replay_includes(
) -> Result<(), Box<dyn std::error::Error>> {
    for access in [WebSearchAccess::Cached, WebSearchAccess::Live] {
        for filter in [DomainFilter::Allow, DomainFilter::Block] {
            let spec = WebSearchSpec::new(access)
                .with_context(WebSearchContext::High)
                .with_domains(filter, vec!["docs.example.com".into()])?;
            let requests = [
                ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
                    model: model("openai", "fixture"),
                    input: vec![],
                    prompt_cache_key: None,
                    max_output_tokens: None,
                    tools: vec![tool()],
                    web_search: Some(spec.clone()),
                    tool_choice: None,
                }),
                ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
                    model: model("openai", "fixture"),
                    input: vec![],
                    prompt_cache_key: None,
                    tools: vec![tool()],
                    web_search: Some(spec),
                    tool_choice: None,
                    reasoning: Default::default(),
                    turn: Default::default(),
                }),
            ];
            for request in requests {
                let wire = body(&request);
                assert_eq!(wire["tools"][1]["type"], "function");
                let key = match filter {
                    DomainFilter::Allow => "allowed_domains",
                    DomainFilter::Block => "blocked_domains",
                };
                assert_eq!(
                    wire["tools"][0],
                    json!({"type":"web_search",
                    "external_web_access": access == WebSearchAccess::Live,
                    "search_context_size":"high", "filters":{key:["docs.example.com"]}})
                );
                assert_eq!(wire["tools"].as_array().map(Vec::len), Some(2));
                let expected = if matches!(request, ProviderRequest::ChatGptCodex(_)) {
                    json!([
                        "reasoning.encrypted_content",
                        "web_search_call.action.sources"
                    ])
                } else {
                    json!(["web_search_call.action.sources"])
                };
                assert_eq!(wire["include"], expected);
            }
        }
    }
    Ok(())
}

#[test]
fn disabling_search_keeps_the_function_only_wire_contract() {
    let response = ProviderRequest::OpenAiResponses(OpenAiResponsesRequest {
        model: model("openai", "fixture"),
        input: vec![],
        prompt_cache_key: None,
        max_output_tokens: None,
        tools: vec![],
        web_search: None,
        tool_choice: None,
    });
    let codex = ProviderRequest::ChatGptCodex(ChatGptCodexRequest {
        model: model("openai", "fixture"),
        input: vec![],
        prompt_cache_key: None,
        tools: vec![],
        web_search: None,
        tool_choice: None,
        reasoning: Default::default(),
        turn: Default::default(),
    });
    assert!(body(&response).get("tools").is_none());
    assert!(body(&response).get("include").is_none());
    assert!(body(&codex).get("tools").is_none());
    assert_eq!(
        body(&codex)["include"],
        json!(["reasoning.encrypted_content"])
    );
}

#[test]
fn search_access_changes_request_identity_without_disturbing_the_function_prefix() {
    let request = OpenAiResponsesRequest {
        model: model("openai", "fixture"),
        input: vec![],
        prompt_cache_key: None,
        max_output_tokens: None,
        tools: vec![tool()],
        web_search: Some(WebSearchSpec::new(WebSearchAccess::Cached)),
        tool_choice: None,
    };
    let mut live = request.clone();
    live.web_search = Some(WebSearchSpec::new(WebSearchAccess::Live));
    let cached = ProviderRequest::OpenAiResponses(request.clone());
    assert_ne!(
        crate::prefix_digest(&cached, 0),
        crate::prefix_digest(&ProviderRequest::OpenAiResponses(live), 0)
    );
    let mut expanded = request;
    let mut later = tool();
    later.name = "activated_later".into();
    expanded.tools.push(later);
    let before = body(&cached);
    let after = body(&ProviderRequest::OpenAiResponses(expanded));
    assert_eq!(before["tools"][0], after["tools"][0]);
    assert_eq!(before["tools"][1], after["tools"][1]);
    assert_eq!(after["tools"][2]["name"], "activated_later");
}
