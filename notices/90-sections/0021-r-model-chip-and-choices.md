### R model chip and choices

`crates/secode-tui/src/models.rs` adapts catalog display names and selected-row
visibility from `openai/codex`, `codex-rs/tui/src/model_catalog.rs` and
`resume_picker.rs`, commit `c465563aa9626d99420062135b187d299f904f81`
(Apache-2.0; Copyright 2025 OpenAI). Grapt keeps its bounded typed catalog
and common Client model-selection API, including offered effort validation.
The retained upstream license and NOTICE in `licenses/openai-codex/` apply.

