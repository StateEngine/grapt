### Incremental open code fences

`crates/secode-tui/src/markdown/fence.rs` adapts conservative open-fence
detection and append boundaries from `openai/codex`,
`codex-rs/tui/src/streaming/code_fence.rs` and `streaming/render.rs`, commit
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025 OpenAI).
Grapt appends plain wrapped lines and sends known completed fences through
its existing bounded highlight worker. Possible closers and normalized source
retain canonical Markdown parsing. License and NOTICE remain in
`licenses/openai-codex/`.

