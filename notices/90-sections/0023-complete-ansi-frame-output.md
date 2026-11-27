### Complete ANSI frame output

`crates/secode-tui/src/output.rs` and `present.rs` adapt the queued
cursor/frame-boundary approach from `openai/codex`,
`codex-rs/tui/src/custom_terminal.rs`, commit
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025 OpenAI).
Grapt retains Ratatui's backend and bounded standard output buffer, deferring
its intermediate ANSI flushes until the synchronized frame closes. Legacy
Win32 command ordering is preserved. License and NOTICE remain in
`licenses/openai-codex/`.

