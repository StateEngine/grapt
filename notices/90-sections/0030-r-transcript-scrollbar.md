## R transcript scrollbar

The overflow-only gutter and viewport thumb in
`crates/secode-tui/src/transcript/scrollbar.rs` adapt `xai-org/grok-build`,
`crates/codegen/xai-grok-pager-render/src/render/scrollbar.rs` and
`crates/codegen/xai-grok-pager/src/scrollback/scrollback_pane.rs`, commit
`2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8` (Copyright 2023–2026 SpaceXAI,
Apache-2.0; retained license `licenses/xai-grok-build/LICENSE`). Grapt uses
its existing palette and cached row geometry, without measuring
unseen history or adding a paint timer.

