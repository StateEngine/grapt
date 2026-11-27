### Provider stream watchdog scope (SC-REL-14)

`crates/secode-runtime/src/step.rs` and `step/driver.rs` adapt the stream
idle lifetime boundary from `openai/codex`,
`codex-rs/codex-api/src/sse/responses.rs`, commit
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0; Copyright 2025 OpenAI).
Modified by Grapt: the existing committed provider terminal ends the
watchdog while owned tools retain their execution bounds and cancellation.
The retained Apache license and upstream notice are shipped with this project.

