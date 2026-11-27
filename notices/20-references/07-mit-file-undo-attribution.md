## OpenCode file-undo attribution

The earliest-before-image composition in
`crates/secode-product/src/dogfood/tools/coding/checkpoints/attribution.rs`
adapts the per-message patch reduction in `anomalyco/opencode`,
`packages/core/src/session/revert.ts`, commit
`042d0832f5f3a73eaa039a58a7d565776f6d4290`. Grapt uses its existing
artifact and checkpoint owners, exact postconditions and a separate native
file-undo command. The upstream license follows unchanged.

```text
MIT License

Copyright (c) 2025 opencode

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

Reasoning effort selection adapts `xai-org/grok-build` `crates/codegen/xai-grok-pager/src/slash/commands/effort.rs`, `src/acp/model_state.rs`, and `crates/codegen/xai-grok-sampling-types/src/conversation/messages.rs` at `2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8` (Apache-2.0, Copyright 2023-2026 SpaceXAI), and `openai/codex` `codex-rs/tui/src/chatwidget/model_popups.rs` and `codex-rs/models-manager/models.json` at `bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0, Copyright OpenAI). Grapt uses typed offered effort choices and its existing atomic model selection and provider serializers.

Generation-effort projection in `crates/secode-provider/src/effort.rs` and
`crates/secode-product/src/dogfood/continuation/effort.rs` adapts OpenAI Codex
`codex-rs/core/src/session/reasoning_effort.rs` and
`codex-rs/protocol/src/models/configuration_update.rs` at
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0, Copyright 2025 OpenAI).
Baseline/last-override agreement also adapts `anomalyco/opencode`,
`packages/ai/src/effort-updates.ts` at `042d0832f5f3a73eaa039a58a7d565776f6d4290`
(MIT, Copyright (c) 2025 opencode). Grapt reconstructs controls from existing
selection facts, keeps unknown routes on ordinary parameters, and preserves replay
account, credential, route and model bindings. The licenses above apply.

Credential recovery adapts the account-pinned reload/refresh sequence from
`openai/codex` `codex-rs/login/src/auth/manager.rs` and
`codex-rs/core/src/client.rs` at `bb64b894fd67158be0f874872e6d7bfe717e7368`
(Apache-2.0, Copyright OpenAI), and rotation-aware recovery from
`xai-org/grok-build` `crates/codegen/xai-grok-login/src/recovery.rs` at
`2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8` (Apache-2.0, Copyright 2023-2026
SpaceXAI). Grapt keeps its own exact credential grants, bounded source reads
and separately receipted provider attempts; it does not use token fingerprints
or upstream token-lifetime and grace assumptions.

New-profile sign-in import also adapts complete authentication-envelope retention
from `openai/codex`, `codex-rs/login/src/auth/storage.rs` at
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0, Copyright 2025 OpenAI).
Grapt reuses its bounded ChatGPT state owner and private profile staging,
refuses mixed account/access generations, and never imports an existing store.

