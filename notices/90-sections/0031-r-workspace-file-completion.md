## R workspace file completion

R's cursor-local `@file` completion, quoted path insertion and dismissed-token
behavior in `crates/secode-tui/src/mentions{.rs,/token.rs}` are adapted from
`openai/codex`, `codex-rs/tui/src/bottom_pane/chat_composer.rs` and
`codex-rs/tui/src/bottom_pane/chat_composer/{completion_target,popup_state}.rs`,
commit `bb64b894fd67158be0f874872e6d7bfe717e7368`. Copyright 2025 OpenAI,
Apache-2.0; retained license: `licenses/openai-codex/LICENSE`. The bounded finder
and its cancellation core retain their existing upstream attribution.

