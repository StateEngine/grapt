// Anthropic's cache-marker policy must be unnameable on an OpenAI request.
// The Responses family keys its cache by `prompt_cache_key`; a marker policy
// there would be exactly the cross-provider bag the request boundary forbids.

use grapt_provider::{AnthropicPromptCache, OpenAiResponsesRequest};

fn main() {
    let _ = OpenAiResponsesRequest {
        model: todo!(),
        input: Vec::new(),
        max_output_tokens: None,
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        prompt_cache: AnthropicPromptCache::StablePrefix,
    };
}
