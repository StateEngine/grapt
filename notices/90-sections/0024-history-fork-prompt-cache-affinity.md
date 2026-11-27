### History-fork prompt cache affinity

`crates/secode-product/src/dogfood/cache_scope.rs` adapts parent conversation
cache affinity from [OpenAI Codex CLI](https://github.com/openai/codex),
`codex-rs/core/src/client.rs`, commit
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0, Copyright 2025 OpenAI).
Grapt resolves immutable retained-history ancestry through Session's read view
for both the body cache key and ChatGPT session-id affinity header, while
preserving each child's actual thread-id and client-request identity.

