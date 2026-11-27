### MCP URL consent and startup elicitation

URL request variants and distinct content-free actions in
`crates/secode-mcp/src/elicitation.rs` adapt `xai-org/grok-build`,
`crates/codegen/xai-grok-tools/src/mcp_elicitation/types.rs`, commit
`2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8` (Apache-2.0;
Copyright 2023-2026 SpaceXAI). The pinned root LICENSE and tools-crate
THIRD_PARTY_NOTICES.md were inspected; that revision has no root NOTICE.
No bundled tools or dependencies from that repository are copied.

Live ownership, cancellation and paused-deadline mechanics in
`crates/secode-product/src/dogfood/elicitation.rs` and
`crates/secode-product/src/dogfood/mcp/startup.rs` and
`crates/secode-mcp-host/src/{startup,channel,connection,startup_lease}.rs`
adapt `openai/codex`, `codex-rs/codex-mcp/src/elicitation.rs` and
`codex-rs/rmcp-client/src/{elicitation_client_service,rmcp_client}.rs`, commit
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0;
Copyright 2025 OpenAI). User-facing URL consent in
`crates/grapt-tui-forms/src/draw.rs` adapts the same revision's
`codex-rs/tui/src/bottom_pane/mcp_server_elicitation.rs`.
The pinned root LICENSE and NOTICE were inspected before adaptation.

Modified by Grapt: existing location-owned durable global forms, bounded
validated HTTP(S) destinations, local IDs separate from server elicitation IDs,
manual-only accept/decline/cancel, no browser or automatic replay, finite startup
human waits, capability/revision checks, and nonblocking catalog acquisition.
The Apache-2.0 license is included in LICENSE-APACHE.

Applicable upstream NOTICE:

OpenAI Codex
Copyright 2025 OpenAI

This project includes code derived from Ratatui, licensed under the MIT license.
Copyright (c) 2016-2022 Florian Dehau
Copyright (c) 2023-2025 The Ratatui Developers
No Ratatui implementation code is copied by this adaptation.
