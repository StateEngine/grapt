//! Select the advertised patch interface without changing dispatch or replay.
// Copyright 2025 OpenAI. Apache-2.0.
// Modified by Grapt: filter existing definitions by the captured route family.
// . License and NOTICE are retained
// in licenses/openai-codex/.

use crate::dogfood::instructions::BuiltinEpoch;
use grapt_provider::{ProtocolFamily, ToolDefinition};

pub(in crate::dogfood) fn offered_definitions(
    family: ProtocolFamily,
    epoch: &BuiltinEpoch,
) -> Vec<ToolDefinition> {
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
    // Retired names stay callable: dispatch never consults this list.
    let retired: &[&str] = if epoch.retires_tools() {
        &[super::READ_LINES_TOOL, super::SUBAGENT_TOOL]
    } else {
        &[]
    };
    let mut tools = super::definitions();
    tools.retain(|tool| tool.name != withheld && !retired.contains(&tool.name.as_str()));
    tools
}

/// An ephemeral Session always starts now, so it takes the current epoch.
pub(in crate::dogfood) fn ephemeral_definitions(family: ProtocolFamily) -> Vec<ToolDefinition> {
    let mut tools = offered_definitions(family, &BuiltinEpoch::Current);
    tools.retain(|tool| super::ephemeral::ensure_supported(&tool.name).is_ok());
    tools
}
