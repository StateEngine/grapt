### Native conversation frontend (SC-TUI-BAKEOFF R)

`crates/secode-tui` adapts redraw merging, committed/live history, streaming
drain and width-keyed Markdown cache patterns from `openai/codex`,
`codex-rs/tui/src/tui/frame_requester.rs`, `streaming/commit_tick.rs`,
`streaming/render.rs`, `history_cell/markdown_render_cache.rs`, `app/history_pagination.rs` and
`transcript_view.rs`, commit
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025 OpenAI).
The existing license and NOTICE under `licenses/openai-codex/` apply.

The cached output key and viewport cache lifetime, including the bounded
painted-row cell cache in `crates/secode-tui/src/transcript/raster.rs`, adapt
`xai-org/grok-build`, `crates/codegen/xai-grok-pager/src/scrollback/entry.rs`
and `blocks/markdown_content.rs`, commit
`2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8` (Apache-2.0;
Copyright 2023-2026 SpaceXAI). The upstream license is retained under
`licenses/xai-grok-build/`. These adaptations use ratatui 0.30, bounded
channels, an injected scheduling boundary, the TS TUI semantic palette and
Grapt's existing typed protocol client.

