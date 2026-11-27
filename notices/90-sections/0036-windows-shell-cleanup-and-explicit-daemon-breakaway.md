## Windows shell cleanup and explicit daemon breakaway

`crates/secode-product/src/dogfood/tools/terminal/windows_job.rs` adapts the
explicit-breakaway/kill-on-close policy from OpenAI Codex,
`codex-rs/utils/pty/src/win/job.rs`, commit
`c465563aa9626d99420062135b187d299f904f81` (Apache-2.0; Copyright 2025 OpenAI).
Modified by Grapt: pinned safe adapters, one observed job, bounded exit confirmation.

The Windows-only adapters are `win32job` 2.0.3 (MIT OR Apache-2.0,
<https://github.com/ohadravid/win32job-rs>) and `filedescriptor` 0.8.3
(MIT, <https://github.com/wez/wezterm>). Their license texts and resolved
transitive dependencies are included in `licenses/dependencies/NOTICE.html`.
