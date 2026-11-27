//! IBM native chat projection; the project comes from the configured route.
//! Adapted from IBM/watsonx-ai-provider at f0021a6511cad5c15a73ddbe351f3e3495f0f8bc.
//! See licenses/ibm-watsonx/ and notices/10-adoptions/watsonx.md.
use crate::{CompatiblePromptCache, OpenAiCompatibleRequest};
use grapt_types::{WatsonxProjectId, WATSONX_API_VERSION};

pub(crate) fn stream_url(base: &str) -> String {
    format!(
        "{}/ml/v1/text/chat_stream?version={WATSONX_API_VERSION}",
        base.trim_end_matches('/')
    )
}

pub(crate) fn body(
    request: &OpenAiCompatibleRequest,
    project: &WatsonxProjectId,
) -> Result<serde_json::Value, &'static str> {
    if request.prompt_cache == CompatiblePromptCache::AnthropicBreakpoints {
        return Err("watsonx does not support Anthropic cache breakpoints");
    }
    if request
        .model
        .variant()
        .is_some_and(|effort| !matches!(effort.as_str(), "low" | "medium" | "high"))
    {
        return Err("choose low, medium or high reasoning effort for watsonx");
    }
    let mut body =
        crate::routes::body::compatible_parts(request, crate::CompatibleProtocol::OpenAi)?;
    body.insert("model_id".into(), request.model.model().as_str().into());
    body.insert("project_id".into(), project.as_str().into());
    body.insert("parallel_tool_calls".into(), false.into());
    Ok(body.into())
}
