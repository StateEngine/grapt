### R failure and recovery feedback

`crates/secode-client/src/projection/feedback.rs`, `crates/secode-client/src/cells.rs` and
`crates/secode-tui/src/recovery.rs` adapt the distinction between transient
retry status and terminal failure from `openai/codex`,
`codex-rs/tui/src/chatwidget/protocol.rs` and
`codex-rs/tui/src/chatwidget/streaming.rs`, at
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025 OpenAI).
Grapt presents normalized service facts in a bounded literal-safe banner,
preserves edited drafts and reuses the existing client submission identity.
License and NOTICE remain in `licenses/openai-codex/`.

ChatGPT client-version negotiation in `crates/secode-login/src/chatgpt.rs` and
`crates/secode-provider/src/transport/adapter.rs` adapts `openai/codex`,
`codex-rs/codex-api/src/endpoint/models.rs`, `codex-rs/model-provider-info/src/lib.rs`,
and `codex-rs/login/src/auth/default_client.rs` at
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0, Copyright 2025 OpenAI).
Modified by Grapt: retain its own originator and user agent and pin Codex
wire compatibility separately from its release version. The retained Apache
license and OpenAI NOTICE in `licenses/openai-codex/` apply.

