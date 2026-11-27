### R live cache and usage presentation

`crates/secode-tui/src/usage.rs` adapts the persistent summary and detailed
status disclosure from `openai/codex`, `codex-rs/tui/src/bottom_pane/footer.rs`
and `codex-rs/tui/src/status/card.rs`, at
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025 OpenAI).
Grapt shares its existing `secode-client/src/view/status.rs` presenter and
provider-reported client projections, with a bounded read-only viewport.
License and NOTICE remain in `licenses/openai-codex/`.

