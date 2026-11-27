### Permission approval previews (SC-API-01)

`crates/secode-api/src/u3a/vocabulary/permission_preview.rs` and
`crates/secode-product/src/dogfood/tools/approval_preview.rs` adapt approval
payloads from `openai/codex`, `codex-rs/protocol/src/approvals.rs` and
`codex-rs/protocol/src/protocol.rs`, commit
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0; Copyright 2025 OpenAI).
Modified by Grapt to use additive typed previews and the existing bounded,
durable permission metadata. The retained Apache license and upstream notice
are shipped with this project.

