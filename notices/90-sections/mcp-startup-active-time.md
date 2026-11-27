### Startup human time through enclosing deadlines

`crates/secode-product/src/dogfood/mcp/{admission,call,nested_startup}.rs` and
`crates/secode-mcp-host/src/{channel,connection,startup,startup_lease}.rs`
reuse the existing scoped active-time implementation in
`crates/secode-mcp-client/src/active_time.rs`. Its original adaptation is
`openai/codex`, `codex-rs/rmcp-client/src/rmcp_client.rs` (`active_time_timeout`),
commit `092d3acd6bec3e3a14bdc7e7a2810ab628ab759d` (Apache-2.0;
Copyright 2025 OpenAI); its existing source attribution is retained.
The startup ownership adaptation remains attributed to the same upstream path
and `elicitation_client_service.rs` at
`bb64b894fd67158be0f874872e6d7bfe717e7368`, whose LICENSE and NOTICE were
inspected for the URL/startup slice. No new upstream source extract is copied.

Modified by Grapt: expose existing pause-aware deadline accounting, share
startup pauses through recovery and queued admission, preserve one cumulative
human allowance, and pause native catalog acquisition only on live startup forms.
The enclosing CodeMode executor observes only startup reservations actually
owned or joined by its active nested MCP calls. Task-scoped bounded observers
retain clocks, not reservation leases; unrelated servers, catalog work and VMs
cannot extend its original active allowance or finite cumulative human ceiling.
Applicable Apache license and OpenAI/Ratatui notices are retained in the existing
Codex notice fragments and `licenses/openai-codex/`.
