# Migration and recovery — operator recipe

For an operator with an **installed artifact and a database**. No Rust toolchain,
no checkout and no workspace are required: every step below runs the delivered
`grapt-migrate` binary.

Nothing here switches a production service. The migration operator refuses
`cutover` by design, and that refusal is not removable from this path.

## The three databases

A rehearsal never touches the database you point it at. It produces two more:

| Name | What it is |
| --- | --- |
| **source** | your existing database, opened read-only and never written |
| **candidate** | a disposable clone, migrated and compared against the source |
| **rollback** | the verified backup restored back to the pre-migration state |

The source is left byte-identical. That is asserted, not intended.

## 1. Rehearse

```
grapt-migrate rehearse \
  --source <database.sqlite3> \
  --run-root <run-dir> \
  --run-id <label> \
  --engine-version <attested-source-commit>
```

`--engine-version` is **not** a cosmetic label. On first Rust write contact it is
written permanently into the database's `engine_epoch` attribution row, and there
is no reverse path (ruling 2026-08-08). Pass the attested source commit of the
binary doing the migration. Never pass a crate version: every crate in this
workspace carries the same `0.1.0` and it identifies no build.

Re-running is safe. The run is journalled and resumes from its last durable
checkpoint; an interrupted run is continued, not restarted.

## 2. Read the eligibility

```
grapt-migrate handoff --source <db> --run-root <run-dir> --run-id <label> \
                       --engine-version <attested-source-commit>
```

Emits `grapt.release.migration-eligibility.v1` on stdout.

To decide whether a receipt is about **this** database, compare
`source_fingerprint`. Do not key on `run_id` — it is a label you chose, and two
unrelated runs can carry the same one.

`outcome` reads `eligible-for-unified-cutover-review`. That is not a cutover
claim. It is input to the transition owner and authorises nothing by itself.

## 3. Check the run at any time

```
grapt-migrate status --run-root <run-dir>
```

`ready_for_handoff` is the only field that means the rehearsal completed.

## 4. Before activating: quiesce, then re-observe the source

**This step is not optional and its order is the whole point.**

Eligibility proves the source had not moved *when the evidence was taken*. It
says nothing about a write landing afterwards. If the incumbent writes to the
source between validation and activation, the migrated candidate does not
contain that write — and activating it discards the write **silently**, because
the loss happens at activation, not at rollback. No recovery disposition will
ever report it.

So the order is:

1. **Stop the incumbent** and confirm it actually stopped. "Timed out" and
   "failed to stop" are not stopped — a process that would not go away is
   exactly the one that may write next.
2. **Only then** read the source's fingerprint again.
3. Hand both facts to the transition. `QuiescedSource::observed_after` refuses
   to exist if the retirement was `TimedOut` or `Failed`, so a transition cannot
   even be attempted against an incumbent that may still be writing.
4. `prepare_database_transition` compares that fresh observation against what
   the rehearsal audited and refuses `SourceMovedSinceItWasAudited` if they
   differ.

Reading the fingerprint *before* stopping the incumbent satisfies the type and
proves nothing: the value can go stale one instruction later. The ordering is
the operator's obligation, and it is what makes the check a proof.

## 5. If activation fails: ask what a rollback costs

```
grapt-migrate disposition --run-root <run-dir> --served <database-in-service>
```

Emits `grapt.release.recovery-disposition.v1`. **Read `restores_everything`
before doing anything else.**

* `rollback-restores-everything` — nothing was durably recorded since
  activation. Restoring the backup resumes all of it.
* `rollback-discards-post-activation-work` — the served database has moved on.
  Restoring the backup still recovers, but only to the **pre-migration** state:
  everything written after activation is not resumed. The report names the
  activation baseline, what is there now, and where the unresumed work continues
  to live.

This command reads the durable journal rather than re-running the rehearsal, so
it works in the situation it exists for. By the time you need it the served
database has legitimately changed, and resuming a rehearsal over a changed
candidate is refused — correctly — as a resurrection.

If the journal never recorded a baseline, this refuses rather than reporting a
clean rollback. "We did not look" must never render as "nothing was lost".

## 6. Roll back

```
grapt-migrate restore --source <db> --backup <rollback-db> --restored <new-path>
```

The restore **never overwrites**. It writes a new database at `--restored`, so
the one in service and the recovered one both exist and you choose between them
after inspecting both. `--restored` must not alias either input; aliases are
refused before anything is written.

## What is not in this recipe

Driving the election against a live production service. The transition itself —
including the quiescence proof above — lives in `grapt-service`
(`prepare_database_transition`, `QuiescedSource`) and is rehearsed in isolated
installations here. Operating it against the daily driver belongs to the later
cutover and soak campaign, not to this operator.

## Exit codes

`0` with a JSON document on stdout, or `2` with
`{"schema":"grapt.release.operator.v1","outcome":"refused","reason":...}`.
Every refusal names its reason; none of them falls back to a default.
