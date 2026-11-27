// Probe: an Anthropic-only parameter must be unnameable in an OpenAI request.
//
// Contract §"Provider request boundary": "The initial compile-fail gate must
// demonstrate that ChatGptCodexRequest { max_output_tokens: ... } and an
// Anthropic-only thinking field in an OpenAI request do not compile."
//
// Extended thinking is Anthropic's; OpenAiResponsesRequest has no such member,
// so the cross-provider leak the oracle's generic body permits cannot be
// written here.

use grapt_provider::{OpenAiResponsesRequest, ThinkingConfig};

// Attribution note: this probe used to build a real `ModelRef` here. Once base
// model and family became catalog truth, `ModelRef::new` stopped existing and
// that line became an INCIDENTAL error that masked the provider-boundary law
// this probe exists to witness. The model is `todo!()` so the only recorded
// error is the unnameable field.

fn main() {
    let _ = OpenAiResponsesRequest {
        model: todo!(),
        input: Vec::new(),
        max_output_tokens: None,
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        // Anthropic-only; must not be nameable here.
        thinking: Some(ThinkingConfig::Enabled { budget_tokens: 2048 }),
    };
}
