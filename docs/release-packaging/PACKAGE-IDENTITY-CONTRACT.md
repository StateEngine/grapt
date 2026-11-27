# Release package identity — the minimal shared contract

Version 1. Defined by R6-RELEASE-PACKAGING-01. Consumed by
R6-MIGRATION-RECOVERY-01, which reads identity and returns the existing
migration eligibility receipt.

**Every field below already exists in the tree.** Nothing here is a proposed
type. That is deliberate: the joint plan says use existing types before adding
any, and a contract that invents its own vocabulary would make both campaigns
translate between two descriptions of the same bytes.

**A proposed interface is not an accepted dependency.** This document is an
interface NOTICE. Neither campaign consumes the other's unlanded branch; only
named landed commits are released, and until then each side tests its own real
boundary against the accepted baseline.

## The five fields

### 1. Package reference and digest

Per artifact, the shape `scripts/daily-use/assemble_candidate.py` already
writes and `scripts/dogfood/windows.py` already verifies:

    { "sha256": "<64 hex>", "bytes": <int> }

read FROM DISK after the copy, never from the source path. The existing
`packages/desktop/resources/tauri-product-pin.json` carries exactly this shape
for the product binary the desktop attaches, alongside `source_commit` — it is
the precedent, not a new idea.

### 2. Engine version — THE IDENTITY IS `source_commit`

Settled between the two campaigns and verified in the tree rather than
accepted on report. Three candidate strings existed:

    crate version   `0.1.0`   — grapt-cli, grapt-product, grapt-release and
                                grapt-tui ALL carry it at the pinned base, so
                                it cannot distinguish two builds
    desktop version `1.18.8`  — packages/desktop/src-tauri/tauri.conf.json.
                                A different artifact; using it here would be a
                                false attribution
    source_commit             — the commit the BINARY was built from, attested
                                by the governed build receipt, not by whatever
                                checkout assembled it

**`source_commit` is the identity. The crate version may ride along as a
secondary attribute, never as the identity.**

This is a durable constraint, not a naming preference, and it is why the field
is pinned here rather than left to each caller. The string packaging hands an
operator is written PERMANENTLY into the user's database:
`crates/grapt-store/src/epoch.rs` stamps

    CREATE TABLE engine_epoch (epoch INTEGER, engine TEXT, version TEXT, time INTEGER)
    INSERT INTO engine_epoch VALUES (2, 'secode-rust', <engine_version>, …)

from the `--engine-version` argument of `grapt-migrate`. That row is the only
forensic record of which engine took ownership of a database at
`PRAGMA user_version = 2`; a missing, duplicate or wrong row is
`CorruptAttribution` and there is no repair path. A database stamped `0.1.0`
would tell a future investigator nothing at all.

### 3. Schema compatibility identity

    grapt.api.dogfood0.dto.v1   crates/grapt-api/src/u3a/dto.rs
                                 `U3A_DTO_VERSION` — the versioned DTO surface
    secode.product.ready.v2     crates/grapt-api/src/product_health.rs  # grapt-rename: keep
                                 the ready receipt a serving product
                                 publishes and a client reads to attach;
                                 StateEngine reads it too, so it keeps this
                                 spelling until StateEngine accepts both

**THESE ARE PROTOCOL AND READINESS DOCUMENT IDENTITIES. THEY ARE NOT ON-DISK
DATABASE COMPATIBILITY, AND MUST NEVER BE USED AS PROOF THAT A STORE IS
COMPATIBLE.** Corrected here after review caught an earlier draft of this file
implying otherwise; the earlier wording said
`ready.v2` was "the one an installer and a migration check both touch", which
reads as though a readiness receipt could answer a store question. It cannot.
The ready receipt (`PRODUCT_READY_SCHEMA`) says a serving product published a receipt a client
can attach to — nothing about the shape or era of the database underneath it.

Store compatibility is a SEPARATE identity, bound through the migration
eligibility evidence below: `PRAGMA user_version`, the `engine_epoch`
attribution row, and the source/candidate fingerprints. R6-MIGRATION-RECOVERY-01
owns establishing it; this campaign carries it as a distinct field when a package needs it and
never substitutes a protocol version for it.

### 4. Declared entry points

Five binaries exist at the pinned base; four are shippable and one is a
measurement tool:

Enumerated with `cargo metadata --no-deps`, NOT by grepping manifests for
`[[bin]]` — that method misses every auto-bin, and it did: an earlier version of
this list said five and omitted `grapt-engined`.

    grapt                 crates/grapt-cli       the CLI / ACP client
    grapt-server           crates/grapt-product   the product / server
    grapt-tui             crates/grapt-tui       the native terminal client
    grapt-migrate         crates/grapt-release   REQUIRED — see below
    grapt-engined         crates/grapt-engined   SHIPPED, AUXILIARY — read
                                                   the limits below before
                                                   binding anything to it
    cache-usage-benchmark  crates/grapt-product   BENCHMARK, not shipped

**`grapt-engined` IS AUXILIARY AND MUST NOT BE MISTAKEN FOR THE PRODUCT.** It
is the headless composition and status tool. Two limits, both read in its own
source:

- `production_service` seals `ApiRouterBuilder::new(max_modules).seal()` with
  NOTHING registered. Its own doc: "this cut serves no product route, not even
  `/health`." It binds an authenticated boundary over an EMPTY router.
- `service ensure|stop` REFUSE BY DESIGN, with a typed non-zero refusal naming
  the missing authority, pending the adapter-relocation lane.

`grapt-server` is the actual product server. `grapt-engined` is not a recovery
controller and is never wired into an install or recovery success path;
shipping it does not complete service control. **Do not use this empty host as
cutover proof** — use the established product/service composition and the
packaged `grapt-migrate`.

**`grapt-migrate` IS A REQUIRED DELIVERED ARTIFACT**, not an inventory row.
The migration campaign asked for it and the reason holds: the situation a rollback operator
exists for is a FAILED ACTIVATION on an operator's machine. That operator has a
broken install and a backup — not a Rust toolchain, not the workspace, not a
multi-gigabyte `target/`. A rollback path reachable only by `cargo run` inside a
checkout is a developer convenience spelled like a recovery path. It is
currently produced by NO artifact build (see below), which is this campaign's
work.

The migration campaign explicitly made no claim on `grapt-tui`; that one is
required because the preceding campaign accepted it as the product's client.

**Inventories across the rename.** The schema id stays
`secode.release.package.v1`, so a verifier recognises exactly two complete
inventories: the five `grapt*` binaries, or the five pre-rename `secode*` <!-- grapt-rename: keep -->
binaries (with the retired `secode-tui-r` in place of `secode-tui` where a <!-- grapt-rename: keep -->
pre-rename package carried it). `binaries`, `entry_points` and `signing` must
all name the same one; a mix is refused. A current package may also carry
`compat_aliases` (exactly `secode` -> `grapt` and `secode-server` -> <!-- grapt-rename: keep -->
`grapt-server`, `.exe` on Windows) and its `compat_aliases_removal` note. They
are installation copies of signed binaries, not entry points. Upgrade across the
rename with the new package's own binary.

### 5. Per-target signing state

    x86_64-pc-windows-msvc    UNSIGNED — no signing identity in either tree
    x86_64-unknown-linux-gnu  UNSIGNED — and no artifact build exists yet
    macOS                     NOT BUILT — inventoried, no runner

A search for signing material found NO code-signing certificate in either
`CurrentUser/My` or `LocalMachine/My`, and no matching signing variables in the
process environment. `packages/desktop/src/main/release-signing.ts` and
`.github/workflows/publish.yml` DO contain Azure / Apple / Tauri signing
integration — but the workflow's Windows signer is restricted to
the upstream repository, so **inherited secret NAMES are not evidence of our
access**, and this file will not treat them as one. Whether a separate signing
account or certificate exists is unconfirmed; only its location, never a secret,
may be recorded.

There is no `signingIdentity`, no `certificateThumbprint` and no certificate
anywhere in the Rust root or `packages/desktop`; `tauri.conf.json` declares
`bundle.active: false` with no `targets` at all. **Unsigned is reported as
unsigned.** A stub verifier is not signed-release evidence, and no signing
material will be fabricated to close a field.

## What the consumer can rely on today, and what it cannot

RELY ON: the digest shape, the three schema identities, the entry-point names,
and that signing state is reported truthfully rather than optimistically.

DO NOT RELY ON YET, because it is measured absent rather than assumed present:

- a Linux artifact — every `remote-compute/*.json` profile carries
  `"evidence": {"artifacts": []}` and produces no binary;
- `grapt-tui` or `grapt-migrate` in a package — the only artifact build
  (`remote-compute/daily-use-windows-build.py`) ships `grapt-server.exe` and
  `grapt.exe` only;
- any installer bundle on any platform;
- `packages/desktop/resources/tauri-product-pin.json` being current — it pins
  `source_commit 44a17ff6` / `sha256 704e246c…`, which is NOT the accepted base
  `40d82e39`. Updating that pin is a deliberate act this campaign owns, in the
  same commit that adopts a new accepted product, exactly as the pin's own
  `how_to_update` requires.

## What this campaign CONSUMES from R6-MIGRATION-RECOVERY-01

The migration eligibility receipt, which already exists and which I read at the
pinned base rather than taking on report —
`crates/grapt-release/src/handoff.rs`, `MigrationEligibilityReceipt`:

    schema   "grapt.release.migration-eligibility.v1"   (&'static str, constant)
    outcome  "eligible-for-unified-cutover-review"       (&'static str, constant)
    run_id, source_database, source_fingerprint, source_domain_fingerprint,
    candidate_database, candidate_fingerprint, rollback_database,
    rollback_fingerprint, rollback_domain_fingerprint, applied_migrations

emitted as one JSON line on stdout by
`grapt-migrate handoff --source <db> --run-root <dir> --run-id <id> --engine-version <v>`.

**KEY IDENTITY ON `source_fingerprint`, NEVER ON `run_id`.** `run_id` is an
operator-supplied label — any non-empty string — so two unrelated rehearsals can
carry the same one. `source_fingerprint` is the physical bytes of the audited
database, and `source_domain_fingerprint` the canonical domain state exported
from it; those are what the producer re-validates against the live files before
handing anything over, refusing a drifted source rather than returning it stale.

**`outcome` IS NOT A CUTOVER CLAIM.** It reads
`eligible-for-unified-cutover-review` deliberately. The presence of a receipt is
input to the transition owner, never authorization for installation to switch
anything, and this campaign does not own data migration.

The migration campaign has warned that this identity may be lowered into
`crates/grapt-types/src/migration.rs` so `grapt-service` can consume it without
depending on `grapt-release`. The change is stated to be additive — same field
names, same schema string, v1 kept — with the diff shared before delivery.
Recorded here so a parser written against the shape above is not surprised.

Changes to this contract are sent directly to the affected campaign, and
this file is the single place its fields are defined.
