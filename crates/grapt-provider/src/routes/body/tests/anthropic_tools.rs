//! Anthropic tool-choice lowering, including the no-tools boundary.
use super::*;

#[test]
fn anthropic_maps_each_tool_choice_to_its_own_wire_value() {
    // FAILURE half: the four choices must be DISTINCT. A witness for `none`
    // alone passes against a mapping that collapses everything to `none`, which
    // is the same class of defect in the opposite direction.
    let cases = [
        (ToolChoice::Auto, "auto"),
        (ToolChoice::None, "none"),
        (ToolChoice::Required, "any"),
    ];
    for (choice, expected) in cases {
        let request = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
            web_search: None,
            uncached_system_suffix: None,
            effort: crate::EffortPolicy::default(),
            model: model("anthropic", "claude-opus-5"),
            messages: vec![user("Hi").into()],
            max_tokens: bound(16),
            instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
            thinking: None,
            prompt_cache: crate::AnthropicPromptCache::Off,
            tools: vec![tool()],
            tool_choice: Some(choice.clone()),
        });
        assert_eq!(
            body(&request)
                .get("tool_choice")
                .and_then(|c| c.get("type"))
                .and_then(|v| v.as_str()),
            Some(expected),
            "{choice:?} must map to {expected}"
        );
    }
    // And the named form carries the name, not just a type.
    let named = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
        web_search: None,
        uncached_system_suffix: None,
        effort: crate::EffortPolicy::default(),
        model: model("anthropic", "claude-opus-5"),
        messages: vec![user("Hi").into()],
        max_tokens: bound(16),
        instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
        thinking: None,
        prompt_cache: crate::AnthropicPromptCache::Off,
        tools: vec![tool()],
        tool_choice: Some(ToolChoice::Named {
            tool: "read_file".to_owned(),
        }),
    });
    let emitted = body(&named);
    let choice = match emitted.get("tool_choice") {
        Some(choice) => choice,
        None => panic!("a named choice must be emitted"),
    };
    assert_eq!(choice.get("type").and_then(|v| v.as_str()), Some("tool"));
    assert_eq!(
        choice.get("name").and_then(|v| v.as_str()),
        Some("read_file")
    );
}

#[test]
fn anthropic_omits_tool_choice_entirely_when_no_tools_are_offered() {
    // The oracle records why, at the lowering: "Anthropic rejects tool_choice
    // when tools are absent; `none` is only meaningful with tools present."
    // Emitting it beside an absent `tools` is a provider 400 — a request that
    // fails for a reason the caller cannot see in their own inputs.
    for choice in [
        ToolChoice::None,
        ToolChoice::Auto,
        ToolChoice::Required,
        ToolChoice::Named {
            tool: "x".to_owned(),
        },
    ] {
        let request = ProviderRequest::AnthropicMessages(AnthropicMessagesRequest {
            web_search: None,
            uncached_system_suffix: None,
            effort: crate::EffortPolicy::default(),
            model: model("anthropic", "claude-opus-5"),
            messages: vec![user("Hi").into()],
            max_tokens: bound(16),
            instruction_mode: crate::AnthropicInstructionMode::CurrentPrefix,
            thinking: None,
            prompt_cache: crate::AnthropicPromptCache::Off,
            tools: Vec::new(),
            tool_choice: Some(choice.clone()),
        });
        let emitted = keys(&request);
        assert!(
            !emitted.contains(&"tool_choice".to_owned()),
            "{choice:?} must not be emitted without tools: {emitted:?}"
        );
        assert!(!emitted.contains(&"tools".to_owned()));
    }
}
