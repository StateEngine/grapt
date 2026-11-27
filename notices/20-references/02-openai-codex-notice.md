## OpenAI Codex NOTICE

`crates/secode-permission/src/prefix.rs` adapts the exact argument prefix matcher
from `openai/codex`, `codex-rs/execpolicy/src/rule.rs`, at
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0, Copyright 2025 OpenAI).
Grapt bounds configuration construction and borrows parsed argument vectors;
the upstream Apache license and OpenAI NOTICE below apply.

`crates/secode-product/src/dogfood/permission_preset/execution.rs` adapts that
repository's `codex-rs/execpolicy/src/rule.rs` and `policy.rs` at the same commit.
Grapt reads its existing JSON configuration, maps decisions onto permission
effects and grants a compound expression only when every command is covered.

The following is reproduced verbatim from the stable revision above and is also
unchanged at the adopted startup-submission revision:

```text
OpenAI Codex
Copyright 2025 OpenAI

This project includes code derived from [Ratatui](https://github.com/ratatui/ratatui), licensed under the MIT license.
Copyright (c) 2016-2022 Florian Dehau
Copyright (c) 2023-2025 The Ratatui Developers
```

Codex's root Apache license does not relicense its third-party code. Ratatui-derived
TUI code needs its MIT notice and license retained. `third_party/wezterm` has its own
MIT license; `codex-rs/vendor/bubblewrap` carries LGPL terms; `third_party/voice`
contains LGPL and other native-library notices and a Microsoft runtime notice.
Those components are not adopted here. Inspect the exact files and retain applicable
additional license texts and notices before any later adoption.

