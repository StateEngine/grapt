# Disposable service handover

This recipe operates a new disposable installation through `grapt-server`.
Use a normal build for operation. A build with `handover-crash-fixture` is a
fault-injection artifact and must not be used as a production release artifact.

## Run the operator witness

Supply an absolute binary path, a new directory whose parent exists, and the
binary's recorded source/build attribution:

```text
python scripts/service-handover/operate.py --binary <grapt-server> --root <new-disposable-directory> --build <recorded-build>
```

On Linux, invoke the script with `python3`. The script refuses an existing root,
uses deterministic provider mode and fixture credentials, and creates its
database through product initialization followed by explicit migration. It then
starts a real predecessor and invokes the real handover operation. The product
waits for that predecessor's finite serving window to retire it; a process that
remains present is refused. It never kills an incumbent by PID or image name.

The script retains the original product-created database, configuration, binary
digest, predecessor readiness, stdout/stderr and `operator-receipt.json`. Success
requires different predecessor/successor instance identities, preserved fixture
domain state, released registration/readiness, and reaped child processes. The
reported script domain digest covers its four bounded fixture tables; the product
handover independently uses the release owner's canonical domain exporter.

This is a finite witness: two-second serving windows, 100 readiness observations,
20-second command limits and five-second child-reaping limits. No paid provider
requests are submitted. Retained files are evidence; no automatic deletion is
performed on the supplied root.

## Operate an already prepared disposable installation

```text
grapt-server handover --service <root> --run-root <rehearsal-directory> --run-id <label> --engine-version <recorded-build>
```

`--engine-version` must match `product.build` in the installation configuration.
The source is the installation's `database.sqlite3`. The store directive remains
explicit: `migrate` for a database requiring migration, `open-ready` for an
already migrated database. A run label does not establish database identity.

The operation rehearses and verifies recovery evidence, observes the registered
predecessor's absence, acquires the existing election, checks retirement
registration custody, and freshly reads the source. It opens the store only after
that source check. The release owner then compares the actual open store's domain
state with the rehearsal and measures its physical fingerprint. The existing
authenticated health probe and controller CAS admit the successor.

The launch receipt reports the actual serving fingerprint, observed source,
retirement outcome, successor instance, endpoint and registration release.
Ordinary startup now takes the same election before store effects. A concurrent
starter therefore refuses before writing, instead of opening/migrating a database
and discovering contention later. Failed preparation drops election custody and
preserves the registration.

## Recover after an interruption

Inspect the actual database and registration, not the last process exit code.
Before migration, rerunning the same handover can continue its verified rehearsal.
After migration, the old source fingerprint may no longer match: refusal is
intentional and does not roll back the store or fabricate an incumbent.

For an intact Rust-owned store, explicitly select `open-ready` and use ordinary
`serve --service <root> --port 0`. Restart admission observes the dead predecessor
and retires only its matching readiness receipt. A live or unattributable owner,
corrupt store, or foreign readiness receipt remains a refusal.

For backup inspection/restoration, use the existing
[migration recovery recipe](../migration-recovery/OPERATOR-RECIPE.md). Restore to
a separate destination and retain the activated database. Restoration may discard
postactivation work; the migration operator's disposition reports that loss.
This campaign adds no reverse migration or automatic backup replacement.

## Leave the slot empty after host loss

Capture the intended owner's `registration` file before stopping it, or retain
the exact registration from the failed run. To release that registration without
starting a successor, invoke:

```text
grapt-server retire-service --service <absolute-profile> --expected-registration <captured-record>
```

Supply the profile's `username:secret` Basic credential through the process's
stdin pipe, with an optional final newline. Credentials never belong in argv.
The command authenticates against the profile, holds its lifetime election and
compares the complete captured registration bytes, including the slot,
environment, instance, generation, endpoint, PID, version and timestamp. The
election proves that the product owner has stopped; a recycled numeric PID does
not establish ownership. A live elected owner, changed registration, malformed
state or foreign ready receipt refuses. This operation opens no database and
neither starts nor signals a process.

Success emits `secode.product.service-retirement.v1` with `outcome: released`,
`registration_released: true` and an empty-slot readback. Only matching ready
custody is retired. If interrupted after that ready receipt was removed but
before the registration was released, retry with the same captured record. If a
reply is lost after release, retry reports `outcome: slot_already_empty` and
`registration_released: false`: it observes the current empty slot rather than
claiming this invocation performed the earlier release. An empty registration
with a remaining ready receipt refuses because its custody is unproven.

## Deterministic crash verification

```text
cargo test -p grapt-product --features handover-crash-fixture --test handover_journey named_crash_boundaries --no-fail-fast -- --nocapture
cargo test -p grapt-product --test handover_journey ordinary_binary_ignores --no-fail-fast
```

The fixture-only selector covers before/after retirement, before/after commit and
restart recovery. Each child exits without destructors at the selected boundary;
the witness checks durable state and reopens through the normal product path.
The ordinary build compiles out both selector and active exit hooks.
