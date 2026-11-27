### Explicit permission bypass

`crates/secode-permission/src/preset.rs` adapts the explicit bypass selection
from `openai/codex`, `codex-rs/cli/src/main.rs`, at
`014489466a1739c2c563cdfe57880c2c7c8680e3` (Apache-2.0, Copyright 2025 OpenAI).
Grapt uses its existing permission layers and preset vocabulary, retains native
tool path confinement, and requires an explicit flag or user-owned configuration.
The retained license and NOTICE in `licenses/openai-codex/` apply.

