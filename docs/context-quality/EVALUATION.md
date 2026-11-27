# CONTEXT-QUALITY-01 evaluation task and live plan

## Bounded task: `cq-repository-task`

One Session, one repository, four user inputs:

1. Objective: `Rename alpha to beta in src/lib.rs and src/util.rs. Never modify NOTES.md.`
2. Delayed correction, after the first compaction: `Correction: rename alpha to gamma instead of beta. NOTES.md stays untouched.`
3. `Report status.` (before a service restart)
4. `Report status again.` (after the restart)

Seed: `src/lib.rs` = `pub fn alpha() {}`, `src/util.rs` = `pub use crate::alpha;`,
`NOTES.md` = `release notes`, and eight 36 000-byte files `cq-1.txt`..`cq-8.txt`. Each
of the first two inputs reads all eight files; with a 128 000 context this forces one
compaction per input, so the task crosses two compactions and a restart.

Critical facts a continuation must keep: the objective, the NOTES.md constraint, the
correction superseding `beta` with `gamma`, and the completed `src/lib.rs` edit (which
must not repeat).

### Deterministic result (loopback, scripted model)

`crates/grapt-product/tests/dogfood_headless/context_quality.rs`
`cq_repository_task_keeps_user_words_and_effects_across_compactions_and_restart`
runs the task through the ordinary product path. The script plays the model, so the
result shows what the harness delivers to the model, not model quality:

- every coding request after the first compaction carries the harness-retained facts;
- the objective's exact words and the completed edit survive both summaries;
- the correction appears after the objective it supersedes;
- the retained facts before and after the restart are byte-identical;
- `src/lib.rs` changed once and `NOTES.md` never.

The `evaluation` route of `remote-compute/context-quality.json` reruns the journeys
with `CQ_EVALUATION_TRACE` set and returns `artifacts/cq-evaluation-trace.json`: every
exact request body, each assistant message's observed usage (scripted values,
labelled so) and the outcome files.

Recorded run `20260929193747-3aa411bba5f44c05` on `9dd1922a`: trace sha256
`19fe94f4fddc2c7bf2de6bddd1e7b1a0b5371351af0d1a693d33c849002c4123`. Outcome: two
compactions; `src/lib.rs` = `pub fn beta() {}` (one edit); `NOTES.md` unchanged. The
post-restart request's retained facts quote both earlier inputs in order and list the 17
settled calls, including `edit {"path":"src/lib.rs","old_text":"alpha","new_text":"beta"}
-> completed`. Usage values in the trace are the fixture's scripted counts.

## Live follow-up (not run; needs an authorized account)

No account or run authority was admitted to this campaign. The plan below is ready
for a separately authorized sample; nothing here is a live quality claim.

- Targets: one Anthropic Messages model and one OpenAI Responses model, exact
  catalog rows, named accounts and credential versions.
- Package: the landed candidate's exact binary on a disposable profile directory;
  discovery overrides the context window to 64 000 so the task compacts at bounded
  cost.
- Runs: 3 per target of `cq-repository-task` (same seed and inputs), plus one cache
  pair per target: the same stable-prefix request twice within 5 minutes, then once
  after changing `AGENTS.md`.
- Bounds: at most 40 provider requests and 30 minutes per run; stop on the first
  provider refusal; sanitized traces only.
- Record per request: body digest, cache markers or key, provider-reported
  input/read/write/output usage (absent stays absent), billed-cost fields when
  present, compaction descriptors, final diff.
- Grade separately: mechanical outcome (`src/*.rs` use `gamma`, NOTES.md unchanged,
  one edit per file), each critical fact present in the model's own next action,
  no repeated completed edit. A cache hit requires a positive provider-reported read
  on the second request of the pair; savings require billed evidence. Report all
  runs, including failures, with the sample size.
