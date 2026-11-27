### Compaction working set

`crates/secode-product/src/dogfood/continuation/summarization/working_set.rs`
adapts newest-first bounded retention from `openai/codex`,
`codex-rs/core/src/compact.rs`, commit
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025 OpenAI).
Grapt retains complete exchanges and uses its admission calibration and
existing structural ceilings. The upstream license and NOTICE are retained
in `licenses/openai-codex/`.

