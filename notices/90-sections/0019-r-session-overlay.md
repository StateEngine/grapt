### R session overlay

`crates/secode-tui/src/sessions.rs` adapts selection visibility, paging and
search from `openai/codex`, `codex-rs/tui/src/resume_picker.rs` and
`resume_picker/layout.rs`, commit `c465563aa9626d99420062135b187d299f904f81`
(Apache-2.0; Copyright 2025 OpenAI). Grapt uses its already bounded typed
session snapshot, stable identity selection and existing input/draw owners.
The retained license and NOTICE in `licenses/openai-codex/` apply.

