### Fork instruction baselines (SC-REL-15)

`crates/secode-instruction/src/service/history.rs` adapts child context baseline
reconstruction from `openai/codex`, `codex-rs/core/src/agent/control/spawn.rs`,
commit `bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0;
Copyright 2025 OpenAI). Grapt snapshots frozen effective instructions when
the child's event numbering restarts, using its existing journal fold and cache
projection. The retained Apache license and upstream notice apply.

