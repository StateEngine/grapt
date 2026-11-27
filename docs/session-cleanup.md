# Offline session-retention cleanup

`grapt-server cleanup-sessions` is conservative retention management, **not
physical erasure**. Stop the product service first. Select its service directory
explicitly; this command does not discover or contact a running service.

```text
grapt-server cleanup-sessions --service /absolute/service/root --cutoff 1790812800000 --current-session ses_keep
grapt-server cleanup-sessions --service /absolute/service/root --cutoff 1790812800000 --current-session ses_keep --apply
```

On Windows, use an absolute Windows service path instead. `--cutoff` (also spelled
`--cutoff-ms`) is a nonnegative integer of Unix milliseconds, strictly before the
injected current time. Only activity **strictly less than** the cutoff qualifies;
a timestamp equal to the cutoff is retained. The example cutoff is October 1,
2026 at 00:00 UTC. Choose the actual cutoff for your retention policy; there is
no implicit age, current-session selection, or automatic scheduled sweep.
`--current-session` is a mandatory typed SessionId exclusion, even while offline.
The parser rejects positional, unknown, repeated, and missing options. Resume,
existing-session execution, and background options are not cleanup options.

## Dry-run and apply

Without `--apply`, the command reports decisions without changing the database.
It acquires the same `registration.elected` lock through
`ElectedControllerAuthority::acquire(root/registration, ...)` as product startup;
the election adapter can create that lock file. Dry-run classifies and opens the
existing database **read-only**, then uses a bounded read snapshot. It does not
initialize, migrate, recover a hot journal, install tables, register a session,
load provider credentials, or write a report/log file. Normal SQLite read-only
opening is not a promise that the OS never touches filesystem bookkeeping or
SQLite shared-memory coordination files.

Apply requires an existing compatible Ready database, uses owned `OpenReady`
authority only, and rechecks eligibility after owned open. Owned open can perform
its existing auxiliary-schema preparation. Cleanup itself never authorizes
initialization or migrations and refuses missing required owner evidence rather
than repairing it. Every candidate is validated before the first deletion.
Each deletion uses `grapt_session::delete_session`, appending its existing
session tombstone and updating the session-list projection. A failure after an
earlier commit is a partial application, not a rollback; the structured error
contains the already-tombstoned SessionIds. A repeat run retains tombstoned
sessions. An apply request with no eligible sessions makes no writable DB open.

## Conservative worked-session eligibility

Old, inactive **leaf sessions can qualify after native conversation work**;
unused seeded sessions can qualify too. Ordinary input admission/promotion,
assistant messages/content/usage, request diagnostics, native tool calls,
permissions, and session-scoped v1 forms are folded through their owner codecs.
Settled conversation history, instruction deltas, config selections, and
permanent provider accounting do not by themselves prevent a tombstone.

- Use the maximum timestamp across **all** session events, including explicit
  v3 Step completion observations (the event timestamp can have been reserved
  before provider I/O), not creation time or a stale list projection. Recent
  seed timestamps and recent shell activity also prevent cleanup.
- Retain the explicitly excluded current session, deleted sessions, sessions
  with a parent, and every parent with any child, including deleted children.
  Inspect durable seeds, list/legacy lineage and subagent relations, not only
  live list results. A legacy baseline with no corroborating lineage source
  refuses inspection.
- Retain open executions, unpromoted inputs, open assistant steps, unfinished
  tools/nested tools, pending or suspended permissions/forms, operation
  deliveries/followups still owed, quarantined receipts, and incomplete provider
  accounting coverage. A worked session also needs a paired execution terminal
  after its latest work; an execution terminal alone never settles its tools.
- Retain ambiguous process-loss/unknown failure evidence, uncaptured tool
  results, and unsupported event shapes or versions. Legacy conversation
  events, compaction/revert/fork histories and native agent/operation/remote
  mail lifecycles are not certified by these folds and remain conservative
  exclusions, even when some of their records look settled. The inbox owner's
  permanent legacy-synthetic audit table is required evidence: its rows retain
  their named sessions, and cleanup never removes or rewrites the audit bytes.
- A retained attempt receipt must decode, agree with **every** indexed column,
  have ordered markers and corroborate its exact session/Step terminal before
  it can be treated as historical. Missing/unsettled/unproven or recent
  settlement retains the session; malformed evidence refuses the pass. Cleanup
  never reclaims receipts or changes permanent accounting or coverage evidence.
- A shell needs a terminal status, settled pipe drain, complete cleanup and old
  timestamps. Unresolved/recent operations are attributed through their public
  binding; an operation without a trustworthy SessionId retains all sessions.
- Every retained seat-mailbox row still protects its attributable sessions:
  consumed/acknowledged flags cannot prove that private binary metadata-repair
  debt is settled. An unmappable recipient retains all sessions; an empty
  mailbox adds no exclusion. This is a deliberate safety limitation, not a
  claim that all settled mail is eligible for cleanup.
- Missing required tables, malformed/unreadable/oversized evidence, unfamiliar
  session-bearing tables/views, and exhausted scan budgets refuse the whole
  pass. Invalid ordering, duplicate starts/terminals, missing seeds, sequence
  gaps and event/cursor-tail disagreement are not invented away. Known history
  tables are allowed selectively, not treated as unknown pending work.
  Limits are 10,000 sessions, 100,000 returned rows across queries, 64 MiB of
  returned text/record bytes, 128 statements/SQL objects, 128 columns per
  inspected object, and 64 KiB per event or canonical receipt payload. The
  read-only snapshot additionally enforces the store's SQL/VM budget.

These safeguards can still produce no eligible sessions in a used profile. Do
not remove operational evidence or weaken exclusions to force a nonempty
result. This command does not resolve effects, terminate children, recover
abandoned executions, or physically purge conversation history.

## Startup and session admission

Startup does not enumerate saved conversation content or digest historical
artifacts. Opening or resuming a session verifies that session's saved content
through the session/type index, under explicit row, item and byte bounds.
Missing or damaged content refuses that session; unrelated sessions remain
available. Successful verification is cached for at most 64 sessions in this
process and is repeated after restart or cache eviction.

Use the offline command above to tombstone eligible old sessions. Tombstones
remove them from ordinary session listings; they do not reclaim database or
artifact space. Physical reclamation needs a separate offline mark-and-sweep
design that retains all references from forks, operational records and backups,
with explicit deletion authority. Never delete artifact directories to shorten
startup: startup no longer reads them globally.

## Ready-store startup

Normal `open-ready` startup validates the file identity, required schema,
engine attribution, quarantine state and migration compatibility. It does not
run a whole-database page scan. Historical corruption is reported when the
selected data is read; a compatible database is not a claim that every saved
page has been audited. Initialization, migration and explicit full read-only
inspection keep their existing integrity checks. Hot-journal recovery still
requires the same owned writable-open authority and full post-rollback
classification; that exceptional recovery can scan historical pages.

The store prepares auxiliary indexes for its schema-probe collision checks
under the existing owned schema transaction. An older profile can pay a
one-time index-build cost; subsequent opens seek those indexes. A foreign or
wrong-shaped namesake refuses instead of being replaced. These indexes do not
change event identities, payloads, write fencing or retention eligibility.

## Coordination and retention limits

Inspection and every tombstone occur while one election guard remains alive.
Product startup must acquire the **same registration authority before opening
its persistent store and retain it throughout the product lifetime**. Cleanup
then refuses a live product, and new products cannot start during cleanup. A
custom launcher that drops or bypasses that guard is unsupported. Independent
SQLite writers, external processes changing files, and surviving user-launched
processes are outside this locking model; stop those before applying cleanup.
The unlocked library `preview` API is diagnostic only, never deletion authority.

Events, artifacts, tool captures, attempt/accounting evidence, SQLite free pages,
WAL/journals, backups, exports, and other retained files are **not erased** by
session tombstones. Intentional repository edits and explicit exports are user
effects and are not undone. Provider-side storage, remote-service retention,
OS caches/swap/crash dumps, and backup retention are outside the command's
control. Cleanup is neither an ephemeral-session implementation nor a provider
zero-retention guarantee.

## Library integration

Export: `grapt_product::session_cleanup::CleanupCommand`. Pass only arguments
after `cleanup-sessions` to `CleanupCommand::parse(argv_strings)`. Then call
`command.run(declared_build, &clock_authority, &id_authority)` **before ordinary
product startup**, without holding a second election authority yourself. The
run returns public `CleanupReport`, `SessionDecision`, `CleanupDisposition`, and
`RetentionReason` values, with SessionIds and timestamps but no transcript text.
Use the install's declared build attribution (for example the read-only
`PublishedConfig::load(root)?.build().as_str()`), not a guessed crate version;
provider/account credential resolution is unnecessary. `preview(connection)`
provides the same read-only classifier for isolated fixtures or diagnostics,
but intentionally acquires no offline lock and never applies tombstones.
