### Mention file finder core

`crates/secode-client/src/file_finder.rs` and its `ranking.rs` submodule adapt
ignore-aware enumeration, path scoring, score/path ordering and query
cancellation from `openai/codex`, `codex-rs/file-search/src/lib.rs` and
`codex-rs/tui/src/file_search.rs`, commit
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0; Copyright 2025 OpenAI).
Modified by Grapt: bounded sequential work, no retained index or worker pool,
no child symlink traversal, one cancellation stamp and frame-independent results.

The approved `nucleo` 0.5.0 and `nucleo-matcher` 0.3.1 dependencies are
MPL-2.0, from <https://github.com/helix-editor/nucleo>. Their unmodified license
texts are retained under `licenses/`; dependency source remains available from
the upstream repository and versioned crates.io archives. `ignore` 0.4.33 uses
the existing workspace license set. Release packaging copies this directory.

