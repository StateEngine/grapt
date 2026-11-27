// A Responses-family cache-routing coordinate must be unnameable on generic
// OpenAI-compatible chat completions. Compatibility claims do not authorize a
// provider-specific Responses field.

use grapt_provider::{OpenAiCompatibleRequest, PromptCacheKey};

fn main() {
    let _ = OpenAiCompatibleRequest {
        prompt_cache: grapt_provider::CompatiblePromptCache::Off,
        model: todo!(),
        messages: Vec::new(),
        max_tokens: None,
        tools: Vec::new(),
        tool_choice: None,
        routing_session: None,
        prompt_cache_key: Option::<PromptCacheKey>::None,
        reasoning_replay: Default::default(),
    };
}
