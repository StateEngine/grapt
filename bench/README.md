# Fixed headless harness benchmark

The suite has 15 fixed tasks in five categories, with B03, R03 and C03 held out.
Development runs use the other 12. Each task/harness pair gets one disposable Git
snapshot, one 10-minute attempt, no retries, and a separate 60-second hidden-test
check after the agent and its descendants stop. Missing, failed and timed-out rows
stay in the denominator. This synthetic suite measures these tasks; it does not
establish general coding superiority.

Two Grapt model routes are configured:

| Cohort | Harness | Model and route |
|---|---|---|
| Luna | Rust CLI | GPT-6 Luna through one ChatGPT account |
| Haiku | Rust CLI | Haiku 5.5 through OpenRouter |

A complete weekly run has 90 attempts, including 30 paid OpenRouter attempts.
The cohorts use different models and billing routes; compare within a cohort and
retain these differences in published results. Native versions and binary hashes,
runner source hashes, corpus hash, requested model and observed model are recorded
separately. The installed Codex 0.159.0 is an inbox fork, not a stock-release claim.

## Prepare and execute

```sh
python3 bench/run.py --output /absolute/new-run-directory --held-out
python3 -m unittest discover -s bench -p 'test_*.py'
```

Without `--execute`, the first command prints the frozen plan and makes no calls.
Use repeated `--harness` flags for a development subset, for example
`--harness codex --harness opencode` for the 24-row Luna development run. Do not
include `--held-out` in development runs. Actual execution requires
`--execute --authorization-ref <received-authorization-message>` and the explicit
native binary paths when their executables are not on PATH.

When a planned cohort must wait, pass `--defer-harness 'claude=<reason>'` to
`run.py` or `automatic.py`. It stays in the planned denominator with that reason;
its executable is neither probed nor launched. A partial execution exits 1 and
reports incomplete measurement. After the constraint clears, run the deferred
cohort as a separate segment and join reports with `report.py`; duplicate attempts
still refuse. Deferral changes no account route, spending authorization or deadline.

The authorized budget is an estimated $21 per weekly run, a $30 run stop and a $70 UTC-week
stop for total OpenRouter usage. The $21 estimate predates the Haiku migration. Recheck the estimate before the first paid attempt
of each run. The shared key stays at its existing provider limit; the runner reads
usage before every paid attempt, every five seconds while one runs, and afterward.
Unavailable or malformed usage stops paid work. Concurrent use of that shared key
counts toward these checks. Report observed shared-key usage separately from
per-attempt token estimates. Provider accounting can settle after a request ends;
these checks cannot promise zero overshoot of an unseen, unsettled charge.

Account input preparation uses `python3 bench/accounts.py --output <new-private-root>`.
It requires default Codex and reference OpenCode to identify the same ChatGPT
account, fetches that account's model discovery, and writes minimal supported
Grapt configuration inputs under private permissions. It makes zero inference
calls and imports no database. Haiku inputs select the supplied OpenRouter key;
the unused primary provider has an inert credential. Never publish or commit
these inputs, native auth files or key material.

Grapt execution additionally needs `--account-source <private-root>` and absolute
`--grapt-binary` / `--product-binary` paths. Its ACP client uses the native Rust
CLI and supported isolated product initialization, explicitly selects the model,
and grants requested tool permissions once. The product retains the provider,
tool, session and state machinery. No daily-driver profile or database is copied.
Before initialization, execution binds Luna inputs to the current native account
and credential, and Haiku inputs to the key monitored by the budget. Selected
inputs are frozen in a private per-attempt copy. The SE Luna import contains only
the access token, with no refresh or ID token; profile credential files are mounted
read-only after the writable profile. Expiry fails the row. Route/key mismatch refuses before
initialization; a key rotation stops paid work rather than resetting its counters.

## Isolation and evidence

Home directories, mounted workspaces under `/mnt`, temporary directories,
other checkouts, corpus and saved results are masked. The agent
sees its snapshot, selected native executable/account file and disposable service
profile. Native auth mounts are read-only; refresh failure is a failed row, not
a reason to sign in or switch to another billing route. Hidden evaluation has no
accounts or networking. Reserved evaluator paths and zero-test successes refuse.
Agents and initialization receive a fixed environment with system PATH, home,
locale, timezone and selected account homes; unrelated keys, backend overrides,
proxies and credential sockets are excluded.
New `agent.jsonl` lines record UTC `received_at` when a complete line reaches the
runner. Native JSON is retained under `event`; non-JSON diagnostics retain their
original bytes under `raw_base64`. This is receiver time, not provider/tool time.
Telemetry accepts both original native logs and timestamp wrappers.
Logs and diffs have a 32 MiB bound; each attempt captures changed and new code,
including files ignored by the agent. Added files are bounded to 4096 entries;
only generated `.pyc` files within `__pycache__` are omitted.
The installed inbox Codex requires its sibling code-mode executable, mounted
read-only and fingerprinted separately. This host refuses nested user namespaces:
Codex uses `--sandbox danger-full-access` inside the shared outer isolation, whose
mount restrictions still bound its writable paths. This configuration is recorded,
rather than attributed to a stock Codex default.

```sh
python3 bench/report.py <run>/results.json
python3 bench/report.py <run>/results.json --markdown docs/benchmarks/<date>.md
```

Unknown usage, cost, observed model and compaction stay null. API-equivalent cost
is separate from subscription spend. OpenCode's reported uncached input and cache
classes are combined for token comparison; a zero native subscription cost is
not treated as a measured API charge. Model selection is not proof of observed
provider model identity.

SE usage is collected after the native row exits, before its isolated profile is
removed. A bounded read-only store snapshot selects the exact ACP Session and
reconciles usage leaves by Physical Attempt and observation phase before summing.
Inclusive input, cache reads, cache writes and output use the same coordinates as
the comparators. C04 includes the compactor's separate provider calls. `usage.json`
retains the selected observations and payload digests; the result pins its digest
and reports `cache_read_share = cached_input_tokens / input_tokens`. Missing,
incomplete, conflicting or legacy ambiguous evidence remains null with a reason;
reported zero remains zero. Historical reports are not silently backfilled.

History forks retain the parent's conversation and usage facts for replay. The
benchmark charges only the fork's own provider calls: copied Physical Attempts
are recognized against the frozen parent sequence in `session.forked`, including
forks of forks. Changed copied usage, missing ancestry or an unsupported fork
boundary makes accounting unavailable. Copied parent usage cannot fill missing
usage from a new child call. The bounded ancestry read shares the same read-only
snapshot and row/byte limits as the child accounting read.

Native SE rows also save `request-cache.json` before removing the private store.
It joins each Physical Attempt (including retries and summaries) to the existing
volatile request comparison: previous attempt, changed input index, change label
and request/component byte counts. The Product exports those allowlisted fields
on its existing bounded dispatch diagnostic; prompt text, hashes, credentials and
routing tokens are excluded. The report retains each provider usage leaf's phase
and provenance, with absent or estimated final counts left unknown. Missing
comparisons, old binaries, restarts and diagnostic loss produce partial coverage;
provider miss causes remain `unreported`, even when a local change is observed.
This report does not change billing totals or establish the 95% seat-day bar.

Weekly summaries include input efficiency per harness and requested model. Uncached
input excludes both cache reads and writes. Uncached/run uses native-completed rows,
including coding failures; uncached/call divides their total by observed Physical
Attempts, including compaction summaries. First-call cached share is the mean of
the completed rows' first-call shares. Codex's whole-run totals do not reveal calls.
Grapt calls follow durable opening order.
Call partitions must reconcile with aggregate usage. Each cell includes coverage;
incomplete coverage makes the aggregate unknown rather than averaging only known rows.

Input-cost-equivalent = uncached + read weight × cache reads + write weight × writes.
The frozen verified catalog in `cache-prices.json` gives read weight 0.1, Anthropic
5m write weight 1.25 and 1h weight 2. OpenAI write weight is zero. The captured catalog's long-context tiers retain these
ratios. Derived fresh/equivalent metrics require `cache_write_tokens_observed`; the legacy
`cache_write_tokens` field may contain a default zero and is never trusted.
Missing write counts or a nonzero Anthropic write's unknown duration makes
the equivalent unknown. Output remains separate; cache share is secondary.
These equivalents compare subscription routes and are not dollar bills.

`Billed $` requires an explicit `provider_billing` record with `usd` and
`provenance: provider-reported`. Current native logs and SE usage do not supply
that per-row evidence, so those cells remain unknown. Native calculated costs and
shared-key budget deltas never count as per-row bills. Future billing collection
needs provider cost evidence; this bench-only change does not alter product usage.

The saved October 7 weekly remains unchanged. Its offline replay adds call metadata
from the original comparator logs and checks every old attempt field and summary
value before producing a separate report/data witness:

```sh
python3 bench/replay_cache.py docs/benchmarks/data/2026-10-07-weekly-complete.json \
  --native-root /home/example/.cache/grapt/weekly-comparators-20261007 \
  --output /tmp/new-weekly-cache.json --markdown /tmp/new-weekly-cache.md
```

Historical Sonnet labels stay Sonnet. SE's historical missing usage stays missing;
OpenCode's missing Anthropic write duration stays unknown. The next scheduled
weekly is October 12, with Haiku for the Anthropic cohort; this replay makes no calls.

Review each captured diff for scope, clarity/maintenance, appropriate complexity
and meaningful validation without test tampering. Score each dimension 0 (material
defect), 1 (adequate), or 2 (strong); record the 0–8 total, reviewer and reasons in
the aggregate report, retaining original attempt records. Publish test results and
quality review coverage separately until all diffs are reviewed.

Long input fixtures currently measure context stress. A file's length does not
prove compaction; only an actual harness event does. The charter's full native harness
coverage remains open; a long fixture alone cannot close it. Do not tune held-out
tasks to manufacture that result.

Codex, OpenCode and Claude Code have separate native manual-compaction probes. They seed
requirements, invoke native compaction, verify its completed native record, and
ask for a coding edit using the retained requirements. The probe uses the same
external namespace/process boundaries and a 600-second limit. Transcripts go to
the outer log, outside agent-visible task files. This is a setup witness, separate
from task scores; it does not prove automatic threshold behavior. Codex may keep
the recent user message during compaction.

```sh
python3 bench/compact.py --harness codex --binary /absolute/codex --lines 0 --output /absolute/new-smoke
# Add --execute --authorization-ref <received-authorization-message> to make calls.
# After the smoke passes, use --lines 1600 and a different new output directory.
# Use --harness opencode for ACP or --harness claude for native stream-json.
# Claude uses the existing Claude Max login and Haiku 5.5.
```

Corpus v2 makes the existing D02 exception-subtype and R02 codec return-type
requirements explicit in their public prompts. Only these two development prompts
changed; held-out records and hidden checks are unchanged. Published development
results retain their original corpus-v1 outcomes, including failures. Diff review
is an unblinded ordinal author assessment, not an independent competitive ranking.

## Earlier development evidence (historical)

All 12 development snapshots fail their hidden tests before correction and pass
with reference solutions. The approved first weekly comparator segment includes all three held-out tasks once; none was tuned or retried. Thirty-eight
no-model checks cover evaluation, masking, evidence, budget refusal, account
alignment and the ACP permission callback. Codex and OpenCode Luna auth smokes
returned actual `BENCH_AUTH_OK` assistant text. The approved comparator segment includes15 paid OpenCode Sonnet rows; observed shared-key usage is reported separately from API-equivalent estimates.

The [corpus-v2 development comparison](../docs/benchmarks/2026-10-07-development-v2.md)
records twelve Grapt development attempts. Two produced nonempty patches
for review; ten native stream failures left empty patches and remained unscored.
Startup and auth/tool smoke passed on accepted build `04cfa2e1`; the stream
correction was still open at that checkpoint.
Only named model-discovery inventories use the product's 16 MiB import bound;
credentials/configuration retain 64 KiB. Those development attempts made no retries or paid/held-out calls; the later approved weekly comparator segment is reported separately.
No complete weekly benchmark is claimed. Native manual-compaction setup evidence
Grapt's supported manual trigger
and coverage remain open.

This slice was ruled Tier 2 development tooling. Ordinary synthetic tests cover
matching and mismatched accounts/routes, credential rotation, environment sentinel
absence and ignored implementation evidence. They make no provider calls. An
affected tooling gate and independent review are used for non-Rust follow-ups;
the original runner's full workspace PASS remains recorded.

On a shared Linux box, use `--io-pressure-threshold 0.5` after measuring an
idle baseline. Each row waits at most 900 seconds for `/proc/pressure/io`
`some avg10` to fall below the selected percentage; the result records that
reading and capacity wait separately from agent wall time. Missing pressure
evidence or an expired capacity wait stops dispatch without inventing an attempt.
Native SE failures retain their disposable service directory under the private
benchmark cache for server diagnostics. Credentials remain private; only the path
is recorded in the result.

Reports separate successful attempts (native completion plus task checks), native
completion, and observed hidden-check passes. Missing evaluator evidence reduces
coverage; it never becomes a hidden pass. Older generated summaries used
`hidden_tests_passed` for joint success; their saved results are retained, while
new reports use the original `test_status` and `test_exit` for task-check counts.

C04 is a separate **development** task in
`fixtures/benchmarks/automatic-compaction.json`. Plan its six model/harness rows
with `python3 bench/automatic.py`. It does not change the frozen 15-task corpus,
held-out tasks, or historical denominators. Its controlled automatic pressure
profile must be reported separately from default settings. Grapt must
receive the released supported `compaction.auto/buffer/keep.tokens` configuration;
the tooling refuses to invent one. Configuration support and actual threshold
behavior still need native setup witnesses after the operator releases a run.

The fixed session admits the initial migration requirements, twelve 24 KiB
archived review messages, and a final coding request. Archive blocks are user
inputs, so tool-output truncation cannot silently remove the pressure. The
rough planned unique volume is 64–96K tokens, not observed tokenizer usage.
Only the initial prompt carries the four active requirements. After the archives,
the final request reveals a new `migrate_many(batches)` stage that flattens batches
using those early rules, without repeating them. The driver records source hashes
and refuses a stage already declared before that reveal; coverage also requires
the stage to appear in changed source afterward. The agent cannot see the hidden
evaluator. Hidden tests check both migration stages, order, fresh outputs, invalid
rows, input preservation and a protected file. Finishing the first stage early
cannot pass the final evaluator alone.

C04 requires a completed **automatic** compaction before the final coding
request, new final-stage source work, successful continuation in the same native Session, and hidden-test
success. Grapt also requires observed live assistant updates. Public history completed
`reason:auto` rows supply evidence. Native IDs are deduplicated. Missing evidence
stays null, a complete census with no automatic compaction gives zero, and both
fail coverage even if hidden tests pass. No compact command or RPC is sent.
Raw evidence is retained in `agent.jsonl`; the report records compaction IDs,
triggers, pre-final boundaries and continuation separately from hidden tests.
SE multi-prompt usage sums the whole Session, including compactor calls, only when
every opened Physical Attempt has complete provider usage. Other multi-prompt
totals remain unknown without a complete native aggregate; an individual final
prompt's usage is not the row total.

Execution needs a **later run release**, explicit native binaries, a
fresh output, account catalogs and the existing weekly budget ledger. Design
approval does not authorize inference. The runner preserves the weekly usage
baseline, stops each paid row at $2.50 and the two paid rows at $5 combined,
and retains the existing $30 whole-run/$70 UTC-week stops. It checks shared-key
usage before a paid row and every five seconds while it runs. Observations may
lag billing; these are observed-use stops, not a guarantee against an in-flight
request overshooting. A persistent allocation ledger and exclusive lock prevent
paid-row retries or reset budgets across segments. All attempts remain bounded
by 600 seconds with no retries; native usage and coverage failures are reported
without silently enlarging the workload. C05 remains deferred.

The pinned OpenCode 1.18.35 config-only witness (network namespace disabled,
no credentials) accepts `auto/buffer/keep.tokens` and normalizes it to
`auto/reserved/preserve_recent_tokens`. Its [exact version's overflow source](https://github.com/anomalyco/opencode/blob/v1.18.35/packages/opencode/src/session/overflow.ts)
subtracts the reservation from `limit.input` when present, and otherwise ignores
the reservation. Native Luna resolution preserves context 272000 and inherits
input 922000; its reservation is therefore 890000, leaving a native count
threshold of 32000. That native count includes input, output and cache tokens.
The binary/configuration-bound witness in `bench/native-model-limits.json`
records the resolved limits. Luna's original limits are preserved.
Native Sonnet resolution has no input limit, so that C04 row is retained as
`uncontrolled: no supported threshold`, with zero provider calls and a coverage
gap. The reservation cannot control it. SE resolves its account inventory window and effective
percentage, defaulting an absent percentage to 95 exactly as its loader does.
Its buffer is effective budget minus 16384 output reserve minus 32000 input:
Luna uses 258400 − 16384 − 32000 = 210016. For Sonnet's captured 1M context,
SE uses 950000 − 16384 − 32000 = 901616. Both captured
inventories omit the percentage; an explicit percentage is honored and recorded.
The report records inventory hash, raw window, effective percentage and budget.

For future Haiku 5.5 rows, the bench provider config explicitly supplies context
1000000, input 1000000 and output 128000. [Anthropic's context documentation](https://platform.claude.com/docs/en/build-with-claude/context-windows)
publishes that window and output ceiling, and rejects input exceeding the window.
The input value maps that published bound into OpenCode's `limit.input`; it is
not a separately advertised input field or a shrunken context window. A fresh
network-disabled native witness resolves all three values, so OpenCode's buffer
968000 controls its native total count at 32000. SE's input-pressure buffer stays
901616. These different native counters remain disclosed; a settings preflight
does not prove a real compaction occurred. Historical Sonnet rows are unchanged.
SE leaves 32000 input without editing the actual model limit. These values
define the settings calculation; native behavioral runs require separate authorization.


## Haiku transition — 2026-10-07

The Anthropic cohort uses Haiku 5.5. New weekly plans use
`claude` on native `claude-haiku-5-5`, plus `opencode-haiku` and
`grapt-haiku` on `openrouter/anthropic/claude-haiku-5.5`. Prepare fresh
`haiku/` inputs; historical `sonnet/` inputs refuse under the new cohort.
Sonnet publications and their model/usage records remain historical. Start a
separate comparison for Haiku; changing a model does not establish a harness gain.
The $30 run / $70 UTC-week stops are unchanged.

[Model IDs, tiered OpenRouter prices and the one-token schema probe](../docs/benchmarks/2026-10-07-haiku-transition.md)
record the migration. The older native Sonnet limit witness above does not
establish Haiku's resolved OpenCode limits. The transition parent's Haiku witness
had no input limit and retained an uncontrolled, zero-call C04 gap. The current
follow-up supplies the published input bound and resolves context/input/output
1000000/1000000/128000; Grapt has controlled settings and pending native
readiness, as described above. Actual compaction still requires the released
native run. SE Haiku uses its fresh account inventory.

## Cache-scope Session probe

`python3 bench/cache_scope_session.py` prints a zero-call plan. After a separate
exact-source GO, invoke it with `--execute --authorization-ref <GO> --binary
<grapt> --product <grapt-server> --service <private-profile> --timeout 600`
inside `run.agent_sandbox`, mounting this driver, `opencode_compaction.py` and
`grapt_usage.py` read-only. Prepare the isolated aligned Luna profile first;
retain its original store and ACP output. The overall window and source/binary
checks belong to the enclosing released runner.

One embedded ACP process creates four fresh Sessions in the fixed project-key
1/2, project-key-and-header 1/2 order. This keeps one service owner and PID
namespace throughout. Only the hidden scope changes before each prompt; its
original config is restored on exit. Each Session selects Luna/medium, verifies
the actual shared ProjectId and canonical root, then sends only READY after the
IO-capacity check. Missing usage stays unknown. A failed first READY skips its
pair; transport, root or placement failures stop all remaining paths. No retries.
The fixed order is descriptive because the second mode inherits cache warmth.

`--initialize-only` creates the four Sessions without prompts. Run that check
with the sandbox network disabled; it exercises native placement and configuration
without spending model calls. It does not establish cache behavior.

## Provider timing diagnostics

Each Grapt result includes `provider_timing` from the volatile dispatch feed
and that session's read-only opening events. `dispatch_prep_ms` measures opening
event creation to dispatch (including its commit), `response_head_wait_ms` measures
dispatch to observed HTTP headers, and `provider_ttfb_ms` measures dispatch to the
first nonempty response chunk observed before decoding. Transport polling and
scheduling contribute to these observations; they do not establish wire arrival
or isolate provider computation. Non-success bodies are not stream observations.
Missing phases, lost samples and inverted timestamps stay explicit or unknown.

For a retained row, run `python3 bench/provider_timing.py --log <agent.jsonl>
--database <service/database.sqlite3> --session <session-id>`. This reads evidence
only. Historical dispatch lines remain usable for preparation intervals with
header/body phases unknown. The existing marker prefix and fields remain intact.
## Direct Anthropic benchmark rows

`automatic.py` and `run.py` select Anthropic direct for paid Claude rows by
default. `--claude-route openrouter` explicitly selects the historical route
for an approved matched comparison. Confirm `--anthropic-model` against the
selected account's model discovery before execution; the Haiku default is
`claude-haiku-5-5`. This selection changes neither the task nor the model window.

Prepare the non-secret direct cohort from the existing approved Haiku account
snapshot, without a provider call:

```sh
python3 bench/accounts.py --direct-anthropic-from /private/accounts/haiku \
  --output /private/accounts/haiku-direct --anthropic-model claude-haiku-5-5
```

The runner loads `/home/example/.config/grapt-bench/anthropic-api.key` into
`ANTHROPIC_API_KEY`. OpenCode uses that selected SDK environment key and an
empty isolated auth record. SE uses its normal Anthropic Messages account and
a transient0600 credential inside the disposable0700 profile. No credential
enters argv, reports or evidence. Native stdout and stderr are redacted before
capture, the row's persisted evidence is checked, and direct private profiles
are removed even after failure. Direct rows cannot retain credential-bearing
diagnostic profiles.

If bounded evidence disposal fails, the whole row moves atomically to a private
quarantine outside the evidence tree, on the same filesystem. The evidence tree
keeps only a sanitized failed result. Cleanup is best effort; a remaining
quarantine prints its path for manual purge and is never included in reports.

Direct execution requires `--anthropic-monthly-ledger /private/anthropic-YYYY-MM.json`.
It is separate from the unchanged OpenRouter weekly ledger. the maintainer's provider
account enforces a hard$200 calendar-month cap with no card; exhaustion stops
Claude rows until the next month. The ledger records each row and a reconciled
`billed_total_usd`. Unknown row billing remains null, and API-equivalent cost
is descriptive rather than billed spend. Reconcile the total with provider
billing; per-run estimates are optional. A ledger for another month refuses
instead of resetting its history.

Every row records route and compaction mode. C04 explicitly passes
`--server-side off|on`; SE records client-summary or provider-server and applies
that setting even if the product default changes. Other native harnesses record
native-client. Ordinary SE rows use the runner's pinned client-summary mode,
or `run.py --compaction-mode provider-server`.

An offline plan grants no live execution: paid C04 still requires the exact
landed source and window release, same-harness smoke, and the existing no-retry
and550/600-second caps.
