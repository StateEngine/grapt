## Encoded message-page cursor cuts

Message history byte-page selection in
`crates/secode-product/src/dogfood/history/page_bytes.rs` adapts returned-row
cursor anchoring from OpenAI Codex,
`codex-rs/app-server/src/request_processors/thread_processor.rs`, commit
`014489466a1739c2c563cdfe57880c2c7c8680e3` (Apache-2.0; Copyright 2025 OpenAI).
Modified by Grapt: exact encoded-byte framing, backward byte cuts and named
single-row refusals. The retained Apache license and upstream notice are shipped
with this project.
