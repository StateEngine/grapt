## Anthropic tool-schema projection

The model-facing tool mapping in `secode-provider/src/routes/body.rs` adapts
`xai-org/grok-build`, `crates/codegen/xai-grok-sampling-types/src/conversation/messages.rs`,
commit `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8` (Copyright 2023–2026 SpaceXAI,
Apache-2.0; retained license `licenses/xai-grok-build/LICENSE`). Grapt flattens
top-level tool-schema composites for Anthropic while retaining their original
constraints for local execution validation.
