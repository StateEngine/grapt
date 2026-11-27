//! Exact compatible body controls for a declared reasoning extension.
use super::*;
use crate::CompatibleReasoningReplay;

fn request() -> OpenAiCompatibleRequest {
    OpenAiCompatibleRequest {
        reasoning_replay: CompatibleReasoningReplay::None,
        model: model("compatible", "model"),
        messages: vec![
            user("question").into(),
            CodexInputItem::AssistantResponseStart,
            InputMessage {
                role: MessageRole::Assistant,
                content: "answer".into(),
            }
            .into(),
        ],
        max_tokens: None,
        prompt_cache: crate::CompatiblePromptCache::Off,
        tools: vec![tool()],
        tool_choice: None,
        routing_session: None,
    }
}

#[test]
fn compatible_reasoning_contract_preserves_ordinary_body_bytes(
) -> Result<(), Box<dyn std::error::Error>> {
    for protocol in [
        crate::CompatibleProtocol::OpenAi,
        crate::CompatibleProtocol::Groq,
    ] {
        let original = request();
        let expected = serde_json::to_vec(&super::super::compatible_body(&original, protocol)?)?;
        let mut with_metadata = original;
        // An older turn without committed reasoning keeps its ordinary bytes.
        with_metadata.reasoning_replay = CompatibleReasoningReplay::ReasoningContent;
        let missing = super::super::compatible_body(&with_metadata, protocol)?;
        assert!(missing["messages"][1].get("reasoning_content").is_none());
        assert_eq!(serde_json::to_vec(&missing)?, expected);
        with_metadata.reasoning_replay = CompatibleReasoningReplay::None;
        with_metadata.messages.insert(
            2,
            CodexInputItem::CompatibleReasoning(CodexToolText::try_new(
                "retained reasoning".into(),
            )?),
        );
        assert_eq!(
            serde_json::to_vec(&super::super::compatible_body(&with_metadata, protocol)?)?,
            expected
        );
        with_metadata.reasoning_replay = CompatibleReasoningReplay::ReasoningContent;
        let body = super::super::compatible_body(&with_metadata, protocol)?;
        assert_eq!(
            body["messages"][1]["reasoning_content"],
            "retained reasoning"
        );
        with_metadata.tools.clear();
        assert!(
            super::super::compatible_body(&with_metadata, protocol)?["messages"][1]
                .get("reasoning_content")
                .is_none()
        );
    }
    Ok(())
}
