### Native mail wake admission

`crates/secode-product/src/dogfood/native_mail/custody/wake.rs` adapts the
mailbox activity and queued input mechanisms from `openai/codex`,
`codex-rs/core/src/session/handlers.rs`, `session/input_queue.rs` and
`session/inject.rs`, at `014489466a1739c2c563cdfe57880c2c7c8680e3`
(Apache-2.0; Copyright 2025 OpenAI). Grapt retains its bounded service driver,
governed input transactions and authenticated binding checks. The license and
NOTICE in `licenses/openai-codex/` apply.


