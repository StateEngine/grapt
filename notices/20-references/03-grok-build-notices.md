## Grok Build notices

Grok Build's root `LICENSE` starts with `Copyright 2023-2026 SpaceXAI` and contains
the Apache-2.0 text reproduced below. There is no root `NOTICE` at the inspected commit.
Its [THIRD-PARTY-NOTICES](https://github.com/xai-org/grok-build/blob/2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8/THIRD-PARTY-NOTICES)
is a dependency inventory with full license texts, not a blanket Apache license.
It includes MPL-2.0 and GPL-2.0 with a libgit2 linking exception alongside permissive licenses.

Additional notices apply to `crates/codegen/xai-grok-tools/THIRD_PARTY_NOTICES.md`,
`crates/codegen/xai-ratatui-inline/NOTICE`, `crates/codegen/xai-ratatui-textarea/NOTICE`,
and `third_party/NOTICE`; fonts and vendored Mermaid code also have separate licenses.
None of those components is adopted here. An adopting slice must reproduce the
notices and license texts applicable to the actual extract and its shipped dependencies.
Do not copy Grok's complete dependency inventory as if Grapt shipped the same set.

