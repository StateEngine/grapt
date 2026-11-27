### MCP local startup recovery and stderr ownership

Local startup backoff in `crates/secode-product/src/dogfood/mcp/retry.rs`
adapts bounded retry mechanics from OpenAI Codex,
`codex-rs/rmcp-client/src/streamable_http_retry.rs`, commit
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0; Copyright OpenAI).
Process-owned stderr drainage in `crates/secode-product/src/dogfood/mcp/stderr.rs`
adapts ownership and the retirement grace from the same upstream commit's
`codex-rs/rmcp-client/src/stdio_server_launcher.rs`.
Modified by Grapt: local-only admission retries, injected clock, cancellation
reservations, fixed-byte reads and private-value redaction before tail retention.
The retained Apache license and upstream notice are shipped with this project.


