### Partial coding searches (SC-TOOL-01b)

`crates/secode-tools-local/src/search.rs` adapts best-effort
entry handling and explicit partial-result reporting from `openai/codex`,
`codex-rs/file-search/src/lib.rs`, commit
`014489466a1739c2c563cdfe57880c2c7c8680e3` (Apache-2.0; Copyright 2025 OpenAI).
Grapt keeps its existing authorized workspace traversal, bounded scan and
omission reporting. The retained license and NOTICE in `licenses/openai-codex/`
apply.


