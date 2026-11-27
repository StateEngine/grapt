# Measure a session or producer day

The Linux R TUI shows the latest request's cache read share and the selected
session's weighted assistant-history share above the conversation. Press F4 for
usage details, context capacity, observed all-call cache totals and available
request-change diagnostics. PageUp/PageDown, arrows and Home/End reveal every
field; Esc or F4 returns to the unchanged draft. This also works after reopening
a session and at 40 columns.

Manual `/compact` first tries to reuse the ordinary coding request's prefix,
appending the summary task after its conversation. If that request cannot be
prepared or does not fit, the summary uses a bounded cold fallback; an unusable
summary can also retry cold once. The first coding request after compaction
replaces the conversation prefix with retained facts and the summary. Later
requests keep that prefix and the compaction input stable, appending new history.
Observed provider usage determines how much cache was actually reused.

A dash is unavailable, never zero or a cache hit. Session history share requires
complete input/read coverage; a bounded, partial history remains incomplete.
All-call totals describe their observed capture, including calls outside the
assistant history, and are distinct from that session share. Missing, unreadable,
lost or overflowed capture data stays explicit. Token counts are the last
available report; provider cache reads describe the latest observed assistant
message. All-call cost uses the pinned API-equivalent pricing policy for each
observed physical attempt, including retries and compaction, with that attempt's
exact model. F4 shows the priced count, missing usage, missing pricing and any
accounting errors. Partial estimates are labelled subtotals; missing usage is
never a free attempt. Subscription charges remain unknown. These capture costs
are separate from assistant-history estimates, which exclude retries and
compaction.

Reopening a session in the same service reads the same capture totals without
adding them again. A service restart loses the volatile capture, so all-call
cost is then unknown; existing assistant estimates and the durable `usage`
export remain available. Use the export below for a day spanning service
restarts or multiple sessions. Capture loss, overflow and missing terminal usage
cannot establish a complete day. Local request differences are evidence, not a
confirmed provider miss cause. These displays do not establish seat-day acceptance.

Session GET/list `cost` is optional. `costStatus` distinguishes
`catalog-api-estimate`, `catalog-api-subtotal`, `unavailable`, `unreadable` and
`known-nonbillable`. API-key estimates reconcile durable Physical Attempts,
including retries and summaries, against the exact model's currently imported
catalog rates in USD per million tokens and its applicable context tier. These
are estimates, not a provider bill or a historical rate snapshot. Missing rates,
usage or unreadable accounting never manufacture zero; a readable Session still
opens. ChatGPT subscription charges remain unavailable even when public API
rates exist. A local nonbillable executor retains its independently known zero.
The separate pinned-policy API-equivalent figures below remain labelled estimates.

Use the Node runtime and Product from the Windows row A package. This command
reads existing durable usage; it does not start a service or call a provider:

```powershell
& 'E:\package\plugin-runtime\node.exe' 'E:\kit\seat-day.mjs' `
  --bundle 'E:\package' --service 'E:\seat-profile' --session 'ses_example'
```

For a day, replace `--session` with explicit UTC boundaries:

```powershell
  --from '2026-10-20T04:00:00Z' --until '2026-10-21T04:00:00Z'
```

Keep the `scripts/cache/*.mjs` files together. The packaged Node 24 runtime
provides SQLite. Window enumeration opens the existing event database read-only;
Product's existing `usage` command owns usage reconciliation and pricing. Run once
per profile, including child profiles if the producer uses them. The index and
session exports are successive snapshots: settle the day before its final export.

The same entry point accepts explicitly selected Codex rollout files:

```sh
node scripts/cache/seat-day.mjs --codex-rollout /path/to/rollout.jsonl \
  --from 2026-11-05T12:00:00Z --until 2026-11-05T20:00:00Z
```

Repeat `--codex-rollout` for additional files. This reads durable per-response
`token_usage_record` usage, including reported incomplete responses and summary
calls, and deduplicates copied response identities. Replayed or estimated
`token_count` snapshots do not contribute. Older files without response records
are refused. The JSON uses the same weighted totals and adds per-root-turn
totals for task comparison; it exports no prompt or tool bodies.
Forks rewrite inherited records with new timestamps. Include the origin's rollout
to attribute its usage to time and provider; inherited-only records retain usage
but have unavailable timing/provider, and appear as unplaced in a time window.

Codex timestamps are usage-record timestamps, not request openings. Dispatches
without recorded usage, retry classification, window-boundary crossings and
billing stay unavailable. The selected turn model/effort is not proof of a
provider-side reroute. These limits remain explicit in the report, so an observed
share alone cannot establish complete seat-day coverage. Use settled task files
and retain the selected-file inventory when comparing producers. The reader is
bounded to 64 files, 256 MiB and 8,192 response identities.

The same JSON includes `operations`: tool calls grouped by name and outcome,
the 50 most frequent normalized failure groups, compactions, failed executions
and failed Steps with their recorded reasons. Tool calls use their durable call
identity across forked sessions; copied parent calls count once. Nested calls
retain their actual tool name. Pending calls and terminals without a recorded
start remain explicit, with unavailable durations or names.
Copied Physical Attempt exports also contribute once, with every containing
session listed. Their Product usage and model coordinates must agree; a partial
or conflicting copy refuses the combined report instead of choosing a total.
Failed Steps deduplicate through the unchanged initial Physical Attempt; an
absent opening remains explicitly unjoined.

`totals.read_share_percent` remains the overall weighted share, including summary
calls and coding after compaction. `operations.compaction_cache` adds the number
of committed and uncommitted compactions and the weighted share for every
Physical Attempt of the first descriptor-bound assistant Step, including retries.
Those attempts enter the window by their own opening time, even when the
compaction preceded the window. Missing usage, openings and first Steps stay
explicit; an empty group has no measured share. Each compaction row retains its
first Step's attempts and summary generation output tokens, bytes and characters.
Generation output can include reasoning and is not the retained summary's input
token count; that count remains unavailable when not recorded by the provider.

Each tool group reports p50 and p95 milliseconds using nearest-rank percentiles.
This measures input identity to terminal (nested admission to terminal for nested
tools), including preparation, permission and queue time. It is not CPU time or
isolated effect time. Calls enter a time window by their start; orphan terminals
use their terminal timestamp. Operational history includes work later rewound
out of the conversation. Failed Steps use their recorded settlement timestamp
when available; failed executions retain the public error type and message.

Compaction rows show every summary attempt, including failed retries and calls
without a committed descriptor. Summary input, cache reads and output come from
Product's existing provider usage export. Context before and after means the
provider-reported inclusive input of the last source-epoch request and first
request bound to the new descriptor. Missing endpoints or usage stay `null`.
Summary length is recorded artifact bytes plus verified Unicode character count
for readable summaries; provider-bound or unavailable text has no character count.

Failure details and summary text are read from existing `dogfood-artifacts`,
checking reference, metadata, identity, length and SHA-256 before use. Secret
artifacts are not read. Missing, unreadable, corrupt and oversized artifacts
produce an explicit unavailable reason while the failed call remains counted.
Normalization removes volatile identities, paths, timestamps and version stamps;
HTTP status and argument-mode guidance remain. Failure previews are limited to
1,024 characters. Treat the report as local diagnostic data.

The operational read is bounded to 131,072 relevant events, 64 MiB of event data,
32,768 distinct tool calls, 1 MiB per artifact and 32 MiB of artifact read budget.
Exceeding an event/call bound refuses the report; exceeding an artifact bound
marks its detail unavailable. Unsupported event versions are listed in coverage
and cannot establish complete tool or failure counts. The command opens SQLite
read-only in both session and window modes and never starts a service.

JSON output contains weighted read share (sum of cache reads divided by sum of
inclusive input), absolute uncached input (inclusive input minus cache reads),
fresh input, cache writes, output and estimated USD cost. Totals and model groups
are observed subtotals; each field has its own unavailable count. Read share and
uncached input use the same complete input/read pairs, with their denominator
reported separately. Every Physical Attempt is listed, including compaction
and retries. A day includes attempts opened in `[from, until)`; opening timestamps
that are absent appear in `window_unplaced_attempts`, not a guessed day.

Cost defaults to the checked-in `api-standard-native-cache-2026-10-27` policy,
with exact entries for `openai/gpt-6.1-sol`, `openai/gpt-6-luna` and
`anthropic/claude-opus-5-5`. Rates were checked on 2026-10-27 against
[OpenAI pricing](https://developers.openai.com/api/docs/pricing) and
[Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing).
These are standard global API-equivalent estimates: OpenAI's 30-minute writes
and Grapt's native Anthropic one-hour writes. Subscription charges, regional
premiums, fast/batch tiers, tool fees and negotiated discounts remain unreported. The two
OpenAI entries refuse input above 272,000 tokens because the pinned band no
longer applies. Custom provider names and model aliases do not inherit rates.

Use `--prices 'E:\exact-model-prices.json'` to override the built-in policy with
verified rates for another account or price band. Every estimate retains its
policy version. Missing pricing or usage stays `unavailable`; an observed zero
remains zero. `seat_day_cost` is available only when all attempts in the selected
window reconcile and have estimates. The separate `totals.cost` retains the
priced subtotal and its unavailable-attempt count. An empty day has no estimate.

The R and standard TUI status pane uses the same Product estimator, with each
assistant row's recorded model and terminal usage. Its cost is explicitly an
assistant-history subtotal; retries and compaction are excluded there and remain
in the durable seat-day report. Partial history and unpriced messages remain
visible. No cost is inferred from a selected model or missing usage.

Each request prints durable, content-free component hashes and an inferred
miss diagnosis when both terminal requests have fingerprints and observed input
and cache-read usage. Fully cached requests report `no-miss`; missing usage stays
unavailable. `tool-list-change`
names a tool hash change; `prefix-break` names changed target, cache key, routing,
instructions, options or input. `new-suffix` means the preceding input is an
unchanged prefix. The longest matching input item boundary and its hash are
reported separately from the component hashes; they are not token positions.
A regressed cache read after an observed idle interval, with no detected prefix
change, is a `ttl-or-idle-gap` candidate. Provider eviction and miss causes remain
unreported; local comparisons do not certify the reason for a cache miss.

The snapshot commits with the existing send permit before the request can open;
it adds no transaction and bypasses the volatile diagnostic queue. Each event is
at most 36 KiB, containing SHA-256 component hashes and at most 512 chained input
prefix hashes, plus attempt and Step coordinates. Authorization bytes and request
text are excluded. The accounting export keeps only hashes and the comparison
boundary, not the per-item ladder. A difference beyond the retained ladder is
explicitly unavailable. First observations, unobserved terminals, missing
predecessors and older rows without fingerprints stay unavailable. Old sessions
cannot recover those comparisons retrospectively.

Missing usage counts, incomplete session enumeration and unplaceable attempts
must remain visible when assessing the 95% bar. Product retains send-permit
evidence after recovery receipts are reclaimed, including attempts that failed
before publishing a message. A finished attempt with no recorded usage has an
explicit unavailable outcome. Complete attempt reconciliation does not make its
missing tokens or price known. Pending attempts and sessions predating accounting
installation remain incomplete; the day report preserves that status. A high
observed share alone does not pass the seat-day bar. No usage
prints unavailable, not a free or perfectly cached day. The command refuses more
than 256 sessions or Product's bounded export failures; never drop requests to
force a complete result. Product reads up to 65,536 events, 8,192 attempts and
64 MiB per session, with a 16 MiB export. An oversized session remains incomplete
or refuses; narrowing the outer time window does not shrink that session read.

The optional policy is the same one accepted by `grapt-server usage --prices`:
`schema` is `secode.price-policy.v1`, `version` is an explicit version identifier,
and `models` contains exact `provider` and `model` strings. Each entry uses
`output_basis: "includes-reasoning"` and the four rates
`usd_per_million_fresh_input`, `usd_per_million_cache_read`,
`usd_per_million_cache_write`, and `usd_per_million_output`. Supply verified
rates for the account and date being measured; absent entries or rates are
unavailable. The report preserves each input field's provenance and observation
phase. Nonterminal observations and incomplete coverage cannot establish a final
seat-day result. Output is bounded to 8,192 attempts and 32 MiB.

Native Anthropic coding requests ask for a one-hour cache lifetime. Their
breakpoints cover the last tool, the stable System block, the previous input
anchor and the conversation tip, using at most four markers. Session identity
remains trusted System context in a separate block after the stable prefix.
Compatible gateways retain their existing cache policy. These request settings
and stable wire prefixes describe reuse eligibility; use provider-reported
usage above to measure the actual read share.

## Disposable producer regression

On Linux, run the ordinary CLI and Product against a private local TLS provider:

```bash
python3 scripts/stateengine-native/cache_producer.py \
  --binaries /absolute/path/to/ordinary-binaries \
  --source <full-binary-source-commit> \
  --scratch /absolute/path/to/new-scratch-directory
```

The binary directory contains `grapt`, `grapt-server` and `source.txt` naming
that source commit. The journey uses bubblewrap, OpenSSL, Git and Python 3,
with a private network, process namespace and home. It reuses the benchmark R01
snapshot-ownership project and the six public CACHE6 prompts; `--task` may name
a retained public CACHE6 preparation JSON for the same continued comparison.
A scripted provider drives real read/edit/shell tools, a dropped-connection retry,
manual compaction and a history fork. Checks require six successful verification
commands, independent snapshot ownership, unchanged coding tool/System prefixes
and cache keys, all 16 physical calls in the ordinary usage export, and clean
shutdown. The separately framed cold manual summary is checked separately.
Every fixture response omits usage: exported input and cache reads must remain
unavailable. This is a local correctness regression, with no real provider calls;
it does not measure model quality, cache read share, cost or seat-day acceptance.
Keep generated requests and reports outside the repository.
