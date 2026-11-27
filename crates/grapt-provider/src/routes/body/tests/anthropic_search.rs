use super::{body, bound, model, system, tool, user};
use crate::{
    AnthropicInstructionMode, AnthropicMessagesRequest, AnthropicPromptCache,
    AnthropicWebSearchSpec, DomainFilter, ProviderRequest, RequestDelta, RequestFingerprint,
    ToolChoice,
};
use serde_json::json;

#[test]
fn anthropic_search_stays_bounded_and_inside_the_stable_tools_boundary(
) -> Result<(), Box<dyn std::error::Error>> {
    for uses in [0, 6, u8::MAX] {
        assert!(AnthropicWebSearchSpec::live(uses).is_err());
    }
    for domains in [
        vec![],
        vec!["https://example.com".into()],
        vec!["example.com".into(); 101],
    ] {
        assert!(AnthropicWebSearchSpec::live(1)?
            .with_domains(DomainFilter::Allow, domains)
            .is_err());
    }
    let request = |search, cache, tools| {
        ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
            web_search: search,
            uncached_system_suffix: None,
            model: model("anthropic", "fixture-model"),
            messages: vec![system("stable").into(), user("search").into()],
            max_tokens: bound(1024),
            effort: crate::EffortPolicy::default(),
            thinking: None,
            instruction_mode: AnthropicInstructionMode::CurrentPrefix,
            prompt_cache: cache,
            tools,
            tool_choice: Some(ToolChoice::Auto),
        })
    };
    for filter in [DomainFilter::Allow, DomainFilter::Block] {
        let spec = AnthropicWebSearchSpec::live(5)?
            .with_domains(filter, vec!["EXAMPLE.COM.".into(), "example.com".into()])?;
        assert_eq!(spec.max_uses(), 5);
        let configured = request(
            Some(spec.clone()),
            AnthropicPromptCache::StablePrefix,
            vec![tool()],
        );
        assert!(crate::routes::request_validation::validate(&configured).is_ok());
        let wire = body(&configured);
        let native = &wire["tools"][0];
        assert_eq!(native["type"], "web_search_20250305");
        assert_eq!(native["name"], "web_search");
        assert_eq!(native["max_uses"], 5);
        let field = match filter {
            DomainFilter::Allow => "allowed_domains",
            DomainFilter::Block => "blocked_domains",
        };
        assert_eq!(native[field], json!(["example.com"]));
        assert!(native.get("input_schema").is_none());
        assert!(native.get("external_web_access").is_none());
        assert!(native.get("cache_control").is_none());
        assert_eq!(wire["tools"][1]["cache_control"]["ttl"], "1h");
        let only_native = body(&request(
            Some(spec),
            AnthropicPromptCache::StablePrefix,
            vec![],
        ));
        assert_eq!(only_native["tools"][0]["cache_control"]["ttl"], "1h");
    }
    let baseline = request(None, AnthropicPromptCache::Off, vec![tool()]);
    let configured = request(
        Some(AnthropicWebSearchSpec::live(1)?),
        AnthropicPromptCache::Off,
        vec![tool()],
    );
    let fingerprint = |request: &ProviderRequest| {
        let wire = body(request);
        RequestFingerprint::of(&wire, &["anthropic"], wire.to_string().len()).ok_or("fingerprint")
    };
    assert_eq!(
        fingerprint(&configured)?.compare(&fingerprint(&baseline)?),
        RequestDelta::ToolsChanged
    );
    assert!(fingerprint(&configured)?.tools_bytes > fingerprint(&baseline)?.tools_bytes);
    assert_ne!(
        crate::prefix_digest(&configured, 0),
        crate::prefix_digest(&baseline, 0)
    );
    let changed = request(
        Some(AnthropicWebSearchSpec::live(2)?),
        AnthropicPromptCache::Off,
        vec![tool()],
    );
    assert_ne!(
        crate::prefix_digest(&configured, 0),
        crate::prefix_digest(&changed, 0)
    );
    let mut duplicate = tool();
    duplicate.name = "web_search".into();
    assert!(crate::routes::request_validation::validate(&request(
        Some(AnthropicWebSearchSpec::live(1)?),
        AnthropicPromptCache::Off,
        vec![duplicate]
    ))
    .is_err());
    Ok(())
}
