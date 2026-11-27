### Concise compaction prompts

The length and continuation guidance in
`crates/secode-product/src/dogfood/continuation/summarization.rs` adapts
`openai/codex`, `codex-rs/prompts/templates/compact/prompt.md`, commit
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025 OpenAI).
Grapt preserves its anchored-summary sections and adds a size target and
one bounded condensation attempt. License and NOTICE remain in
`licenses/openai-codex/`.

- SC-TUI-03: `crates/secode-client/src/markdown.rs` and `markdown/` adapt openai/codex `codex-rs/tui/src/markdown_render.rs`, `wrapping.rs`, `terminal_hyperlinks.rs` and `terminal_hyperlinks/source.rs` at `bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0, Copyright 2025 OpenAI; modified by Grapt). Shared typed frame/sink remains the terminal output owner.
- `pulldown-cmark` 0.10.3 (MIT), `unicase` 2.10.0 (MIT OR Apache-2.0), and `unicode-segmentation` 1.13.3 (MIT OR Apache-2.0): license texts retained under `licenses/`.

