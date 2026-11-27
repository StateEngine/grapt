//! Native Vertex Claude projection over the Anthropic stream decoder.
//! License and attribution notices are retained in THIRD_PARTY_NOTICES.md.
use crate::AnthropicMessagesRequest;
use serde_json::Value;

pub(crate) fn url(base: &str, model: &str) -> Result<String, &'static str> {
    if model.is_empty()
        || model.len() > 256
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.@".contains(&b))
    {
        return Err("Vertex Claude model must be one bounded resource segment");
    }
    Ok(format!(
        "{}/{model}:streamRawPredict",
        base.trim_end_matches('/')
    ))
}

pub(crate) fn body(request: &AnthropicMessagesRequest) -> Result<Value, &'static str> {
    if request.web_search.is_some() {
        return Err("Vertex Claude supports client tools; use the application's web tools");
    }
    crate::routes::body::vertex_anthropic_body(request)
}
