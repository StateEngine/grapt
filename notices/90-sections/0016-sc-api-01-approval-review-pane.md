### SC-API-01 approval review pane

The scrollable approval view adapts `openai/codex`,
`codex-rs/tui/src/bottom_pane/approval_overlay.rs` and
`codex-rs/tui/src/pager_overlay/scrolling.rs`, at
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025 OpenAI).
Modified by Grapt for bounded typed previews and its existing frame/input
owners. `crates/secode-tui/src/permission.rs` adapts the same paging and
approval layout and uses the standard TUI’s literal context formatter. The upstream license and NOTICE remain in `licenses/openai-codex/`.
