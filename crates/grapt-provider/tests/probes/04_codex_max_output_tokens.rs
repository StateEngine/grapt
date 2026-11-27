// Probe 4 (foundational-types contract §"Compile-time ownership probes",
// parked for this crate by grapt-types/tests/compile_fail.rs).
//
// The ChatGPT/Codex backend rejects max_output_tokens. The oracle builds the
// field and then deletes it (openai-responses.ts:122), guarded only by a test
// asserting absence (model-resolver.test.ts:405) — a recurring regression.
//
// Here the field does not exist on that route, so reintroducing it is a
// compile error. The contract: "an assertion that a serializer strips the
// field is weaker."

use grapt_provider::{ChatGptCodexRequest, MaxOutputTokens};

// Attribution note: this probe used to build a real `ModelRef` here. Once base
// model and family became catalog truth, `ModelRef::new` stopped existing and
// that line became an INCIDENTAL error that masked the provider-boundary law
// this probe exists to witness. The model is `todo!()` so the only recorded
// error is the unnameable field.

fn main() {
    let _ = ChatGptCodexRequest {
        model: todo!(),
        input: Vec::new(),
        prompt_cache_key: None,
        tools: Vec::new(),
        web_search: None,
        tool_choice: None,
        // This field does not exist on the Codex route.
        max_output_tokens: MaxOutputTokens::new(4096),
        reasoning: Default::default(),
        turn: Default::default(),
    };
}
