## R workspace slash commands

`crates/secode-tui/src/slash.rs` and command token parsing adapt
`openai/codex`, `codex-rs/tui/src/bottom_pane/{command_popup,prompt_args}.rs`,
commit `bb64b894fd67158be0f874872e6d7bfe717e7368` (Copyright 2025 OpenAI,
Apache-2.0; license and NOTICE retained in `licenses/openai-codex/`).
The bounded, single-pass argument expansion in `slash/expand.rs` adapts
`xai-org/grok-build`, `crates/codegen/xai-grok-tools/src/implementations/skills/skill.rs`
(`apply_substitutions`), commit `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8`
(Copyright 2023-2026 SpaceXAI, Apache-2.0; retained in `licenses/xai-grok-build/`).
Grapt uses its existing workspace catalog, quoted argument grouping and
submission owner, with bounded prompt expansion and no implicit shell execution.

