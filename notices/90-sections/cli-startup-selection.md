## CLI startup model selection

The shared optional model selection for headless and interactive CLI startup is
adapted from `openai/codex`, `codex-rs/utils/cli/src/shared_options.rs`, commit
`bb64b894fd67158be0f874872e6d7bfe717e7368` (Apache-2.0).
Grapt adds an agent choice, typed provider/model coordinates, and forwards both
through its existing atomic Session creation API. Upstream LICENSE and NOTICE
are retained in `licenses/openai-codex/`.
