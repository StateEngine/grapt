`crates/secode-client/src/view/queued_input.rs` adapts the removable pending-input
preview from `openai/codex`, `codex-rs/tui/src/chatwidget/input_queue.rs`, commit
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0, Copyright 2025 OpenAI).
**Modified by Grapt:** derive a small preview from the server's bounded input
list instead of keeping another execution queue. License and NOTICE are retained
in `licenses/openai-codex/`.

