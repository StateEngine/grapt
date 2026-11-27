## CLI JSONL message publication

`crates/secode-cli/src/run/jsonl.rs` adapts completion-based message publication
from `openai/codex`, `codex-rs/exec/src/event_processor_with_jsonl_output.rs`,
commit `bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0; Copyright 2025 OpenAI).
Modified by Grapt: one initial durable page, typed message upserts and a cache
restricted to that page. The license and NOTICE remain in `licenses/openai-codex/`.

