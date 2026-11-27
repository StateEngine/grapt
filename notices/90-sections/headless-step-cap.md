## Headless work limit

Optional nonzero work limits and an explicit exhausted outcome are adapted from
`xai-org/grok-build` at `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8`,
`crates/codegen/xai-grok-agent/src/config.rs`,
`crates/codegen/xai-grok-shell/src/agent/mvp_agent/agent_ops.rs`, and
`crates/codegen/xai-grok-shell/src/session/acp_session_impl/{spawn,turn}.rs`.
Grapt counts logical Steps at its existing admission boundary, preserves the
bound with the prompt, and retains the existing typed budget failure.
Copyright 2023-2026 SpaceXAI, Apache-2.0; license and notices remain under
`licenses/xai-grok-build/`.
