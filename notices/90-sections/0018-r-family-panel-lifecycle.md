### R family panel lifecycle

The snapshot-driven liveness and family navigation in
`crates/secode-tui/src/subagents.rs`, `ui.rs` and `main.rs` adapt
`openai/codex`, `codex-rs/tui/src/multi_agents.rs`, commit
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025 OpenAI).
Grapt retains its typed session snapshots, existing palette/widget and event
owner, and collapses the family panel while idle. License and NOTICE remain
in `licenses/openai-codex/`.

