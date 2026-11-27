//! Select the advertised patch interface without changing dispatch or replay.
// Copyright 2025 OpenAI. Apache-2.0.
// Modified by Grapt: filter existing definitions by the captured route family.
// . License and NOTICE are retained
// in licenses/openai-codex/.

use grapt_provider::{ProtocolFamily, ToolDefinition};

pub(in crate::dogfood) fn offered_definitions(family: ProtocolFamily) -> Vec<ToolDefinition> {
    let withheld = match family {
        ProtocolFamily::OpenAiResponses | ProtocolFamily::ChatGptCodex => super::PATCH_TOOL,
        ProtocolFamily::AnthropicMessages
        | ProtocolFamily::VertexAnthropic
        | ProtocolFamily::WatsonxChat
        | ProtocolFamily::OpenAiCompatible
        | ProtocolFamily::BedrockConverse
        | ProtocolFamily::GoogleGemini
        | ProtocolFamily::VertexGemini => super::APPLY_PATCH_TOOL,
        ProtocolFamily::CohereChat => super::APPLY_PATCH_TOOL,
    };
    let mut tools = super::definitions();
    tools.retain(|tool| tool.name != withheld);
    tools
}

pub(in crate::dogfood) fn ephemeral_definitions(family: ProtocolFamily) -> Vec<ToolDefinition> {
    let mut tools = offered_definitions(family);
    tools.retain(|tool| super::ephemeral::ensure_supported(&tool.name).is_ok());
    tools
}
