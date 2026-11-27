## Original-message compaction retention

`crates/secode-product/src/dogfood/continuation/retained_users.rs` and
`crates/secode-inbox/src/promoted_read.rs` adapt
newest-first user-message selection and the approximate byte-based token budget
from `openai/codex`, `codex-rs/core/src/compact.rs` and
`codex-rs/utils/string/src/truncate.rs`, at
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Copyright 2025 OpenAI, Apache-2.0).
Modified by Grapt: whole messages only, versioned durable replay, separate
settled-tool ledger, and existing conservative full-request admission.
The license and OpenAI NOTICE remain in `licenses/openai-codex/`.

- TUI-R paragraph separation in `crates/secode-tui/src/markdown.rs` adapts openai/codex `codex-rs/tui/src/markdown_render.rs` at `bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0, Copyright 2025 OpenAI; modified by Grapt for retained suffix parsing and shared height measurement). The license and notice are retained under `licenses/openai-codex/`.

