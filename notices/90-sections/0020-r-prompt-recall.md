### R prompt recall

`crates/secode-tui/src/recall.rs` adapts editor snapshot and search lifecycle
from `openai/codex`, `codex-rs/tui/src/bottom_pane/chat_composer/history_search.rs`,
commit `c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025
OpenAI). Matching, bounds and traversal reuse Grapt's existing shared
`composer_history` module, whose original Apache notice remains intact.
The retained upstream license and NOTICE in `licenses/openai-codex/` apply.

