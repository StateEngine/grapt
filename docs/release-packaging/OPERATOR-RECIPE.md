# Release package operator recipe

Reusable. Every command here is one a person actually runs, in order, with the
answer they should see. Nothing in this document requires a Rust toolchain, a
checkout of this workspace, or a `target/` directory — that is the point of
delivering binaries rather than build instructions, and it is verified rather
than promised (see **Why you can trust the toolchain claim** at the end).

The commands are the same ones `scripts/release/exercise_package.py` runs under
the governed `release-linux-journey`, so this recipe and the automated journey
cannot drift into describing different products.

---

## 0. What is in the package, and which binary is which

A release package is a directory containing the shipped binaries, the build
receipt they came with, and `release-manifest.json`.

**Read the roles before you run anything.** `entry_point_roles` in the manifest
carries them, and they are not interchangeable:

| binary | role | what it is |
| --- | --- | --- |
| `grapt-server` | `product-server` | **the product server.** This is the binary a service runs. |
| `grapt` | `cli` | the command-line / ACP client. |
| `grapt-tui` | `terminal-client` | the native terminal client. |
| `grapt-migrate` | `migration-operator` | the migration and rollback operator. |
| `grapt-engined` | `auxiliary` | **auxiliary only.** See the warning below. |

> **`grapt-engined` is not the server.** It is a headless composition and
> status tool. Its `production_service` seals an API router with nothing
> registered, so it serves no product route — not even `/health` — and
> `service ensure` and `service stop` refuse by design with a typed non-zero
> refusal naming an authority it does not issue. It is shipped so the artifact
> set is complete, and it must never be placed on an install or recovery
> success path. If you are looking for the server, it is `grapt-server`.

**Two compatibility aliases, temporarily.** StateEngine still launches
`secode` and `secode-server` by name, so the manifest's `compat_aliases` maps <!-- grapt-rename: keep -->
`secode` -> `grapt` and `secode-server` -> `grapt-server` (with `.exe` on <!-- grapt-rename: keep -->
Windows), and `compat_aliases_removal` says when they go away. They are not
entries in `binaries` and carry no signature of their own: installation copies
each from its target only after the target's signed digest has been verified,
re-hashes the copy against that digest, and removes it with its version on
prune. The installer refuses any other alias, an alias pointing anywhere else,
and aliases in a pre-rename package. A pre-rename version already ships real
`secode` and `secode-server`, so `current/bin/secode` and <!-- grapt-rename: keep -->
`current/bin/secode-server` exist whichever build is active. <!-- grapt-rename: keep -->

**Upgrade across the rename with the new package's own binary.** Run the
extracted package's `grapt update --from <package>`. An installed pre-rename
`secode` given a grapt package refuses at package verification, before any <!-- grapt-rename: keep -->
version is staged or activated; that refusal is correct, not something to work
around.

`cache-usage-benchmark` is deliberately **not** shipped: it is a measurement
tool, not a product artifact. It is named in the manifest's
`excluded_binaries` so that "excluded on purpose" is distinguishable from
"forgotten".

---

## 1. Install

    python3 scripts/release/exercise_package.py \
        --package  <package directory> \
        --work     <a fresh directory OUTSIDE any checkout> \
        --receipt  <where to write the journey receipt>

If you are installing by hand instead, the rules the installer enforces are the
rules you must follow, and each one exists because skipping it has a specific
consequence:

1. **Verify every digest from the manifest before placing any byte.** An
   installer that copies without checking is a copy command, and the manifest is
   the only thing between you and a corrupted transfer.
2. **Check identity, not just digests.** Every binary's attested
   `source_commit` must equal the manifest's `engine_version`, and the package's
   own build receipt must attest the same commit. Digests prove the binaries are
   the ones the manifest describes; they say nothing about whether the manifest
   is describing the right engine. A package assembled from mismatched
   components passes every checksum.
3. **Do not trust a signature claim that has nothing behind it.** `signed: true`
   with no verifier result is refused. See section 5.
4. **You should not need to make anything executable.** The packager sets the
   executable bit, and a package whose binaries cannot be run is a defective
   package rather than a step for you to remember. This used to be an
   instruction here; it was documentation standing in for a defect, and the
   defect is fixed. If you ever do have to restore the bit by hand, that is a
   bug worth reporting.

Expected result: the five binaries in your install prefix, plus a copy of the
manifest.

---

## 2. Confirm the install runs

    <prefix>/grapt-server    --help
    <prefix>/grapt          --help
    <prefix>/grapt-tui    --help
    <prefix>/grapt-migrate
    <prefix>/grapt-engined  service status \
        --registration <dir> --lock-attempts 3 --receipt-root <dir>

`grapt-migrate` answers five verbs — `rehearse`, `handoff`, `status`,
`restore` and `disposition`. `restore` and `disposition` arrived with the landed
R6 migration work, and they are the reason this binary is shipped rather than
left in a checkout: `restore` is the executable rollback, and `disposition`
states truthfully what a rollback would and would not resume **before** anyone
acts on it.

With no arguments it prints its usage as a **typed JSON refusal** and exits 2:

    {"schema":"grapt.release.operator.v1","outcome":"refused","reason":"usage: ..."}

That is the intended answer, not a failure — the refusal is machine-readable
because operators script against it.

`service status` may report that nothing is running. That is a correct answer in
a fresh install; do not read a non-zero exit here as a broken binary.

---

## 3. The refusals you should see, and must not "fix"

These are product behaviour. If any of them stops refusing, something is wrong
with the build, not with your invocation.

    grapt-migrate cutover
      → exit 2, "cutover is owned by the unified R6 release authority;
                 migration operator cannot commit it"

    grapt-engined service ensure  --registration <dir> --lock-attempts 3 --receipt-root <dir>
    grapt-engined service stop    --registration <dir> --lock-attempts 3 --receipt-root <dir>
      → non-zero, naming the elected controller authority it does not issue

    grapt-engined service status --registration <dir>
      → non-zero, "required bound --lock-attempts was not supplied"

The last one is worth understanding: a required bound is **refused, not
defaulted**. A service control tool that quietly invents a bound is more
dangerous than one that stops.

---

## 4. Recovery from a failed install

Keep the package directory. It is your backup, and recovering from it means the
recovery never depends on the broken install it is repairing.

1. Confirm the install is actually broken — run the binary and see it fail.
   Restoring something that was never broken proves nothing.
2. Copy the binary back from the package, and restore the executable bit.
3. Confirm the restored binary behaves: `grapt-migrate` with no arguments must
   again produce the typed `grapt.release.operator.v1` refusal.

### Rolling back the DATA, not just the binaries

The steps above restore the installed binaries. Restoring a store is
`grapt-migrate`'s job:

    grapt-migrate restore     --source <db> --backup <db> --restored <dest>
    grapt-migrate disposition --run-root <dir> --served <db>

Two properties worth knowing before you need them:

- **`restore` never overwrites.** The restored database is a NEW artifact beside
  the one in service, so you inspect both truths before choosing either. That is
  why no confirmation flag guards it.
- **Ask `disposition` FIRST.** It states what a rollback would and would not
  resume, before anyone acts on it, and it reads the durable JOURNAL rather than
  re-running the rehearsal — by the time you want a rollback the served database
  has legitimately changed, and resuming a rehearsal over a changed candidate is
  what the resurrection guard refuses. A missing digest refuses loudly rather
  than defaulting, because "we did not look" must never render as "nothing was
  lost".

`grapt-migrate` is delivered as a binary for exactly this moment. The operator
in a failed activation has a broken install and a backup — not a Rust
toolchain, not this workspace, and not a multi-gigabyte `target/`. A rollback
reachable only through `cargo run` inside a checkout is a developer convenience
spelled like a recovery path.

---

## 5. Signing — what this package does and does not claim

**These packages are unsigned, and they say so.** Every entry in the manifest's
`signing` map carries `"signed": false` with the reason: no code-signing
certificate or signing identity exists in this tree or in the machine
certificate stores.

That is a reported fact, not a gap someone forgot to fill. Two things follow:

- A stub verifier is **not** signed-release evidence, and none is provided.
- An installer must refuse a package claiming `"signed": true` with no verifier
  result behind it. Trusting that claim is exactly how an unsigned package
  becomes one described as signed.

When a signing identity becomes available, the per-target state becomes the real
verifier result and this section changes with it.

---

## 6. Configuring a real provider, and cleaning up afterwards

The package ships no credential and reaches no provider on its own. To make the
product hold a real conversation you supply provider inputs, and the way you
supply them matters as much as the fact that you do.

### What a service root needs

A service root is a directory the product owns. It holds `grapt.jsonc` (the
provider declaration), the credential files that config NAMES, the model
catalog and account discovery snapshots, `service.conf`, and — after first run
— the store. `scripts/native-tui/service.py prepare` builds one correctly and is
worth reusing rather than re-deriving, because it already handles four things
that are easy to get wrong:

1. **Copy the files the config NAMES, not a fixed list.** A config declaring a
   second provider account names its credential and discovery files; copying
   only a hardcoded set leaves the product refusing to boot with
   `openrouter-account-models.json is unreadable`.
2. **The store needs no setting.** The product creates an absent store,
   migrates one with pending migrations behind a verified backup, and opens a
   ready one. A `store.authority` left in `grapt.jsonc` is ignored, and each
   start says it can be removed.
3. **A git boundary at the root.** Without it, project discovery walks up and
   resolves the product's project to whatever repository happens to contain your
   directory.
4. **The product's output goes to files, not to an unread pipe.** A pipe nobody
   drains fills and blocks the writing thread mid-turn. Measured, in one service
   root, changing nothing else: twelve recorded execution failures became zero.

### Handling the credentials

- Stage them **privately**: a `0700` directory, `0600` files.
- Keep them **outside** anything you will publish — artifacts, logs, bundles.
- Never put a secret in a command argument, an environment dump, or a receipt.
  Record inputs by **name and SHA-256 fingerprint** only, and redact endpoints to
  their host.
- Check **route and catalog compatibility first**, from the product's own
  `/api/provider` and `/api/model`. A model the configured account cannot issue
  is discoverable for free; discovering it from a paid refusal is not.

### Cleaning up — this is a step, not an afterthought

    remove the service root          # it holds credential COPIES
    remove the staged input directory

The service root contains copies of your credentials. A task-owned copy of a
provider credential that outlives the task is the thing that goes wrong later,
so `provider_canary.py` removes its service root automatically unless you pass
`--keep-root`, and the staged inputs are removed when the work that needed them
is done.

### The bounds worth keeping even when nobody imposes them

One small, tool-free prompt on a low-cost model, and **no retry loop**. A
provider refusal is a result to report, not something to re-attempt: a retry
loop against a paid endpoint is how a smoke test becomes a bill.

---

## Why you can trust the toolchain claim

Because it is tested rather than asserted. The governed journey runs every one
of these commands with `cargo`, `rustc` and `rustup` removed from `PATH`, from a
working directory outside the source tree — and it then **verifies the scrub
actually took effect**, and verifies the scrub was *meaningful* by recording
that those tools were resolvable before it.

An exercise that ran with a toolchain on `PATH` would pass identically whether
the binaries were self-contained or not. A witness that would hold whether or
not its claim is true is not a witness, so the journey refuses to report a green
it could have produced either way.

## What this recipe does not cover

- **macOS.** Declared nowhere in the build and no runner is available. It is
  inventoried explicitly rather than dropped silently.
- **Production migration.** `grapt-migrate` here is exercised as a delivered
  operator that runs and refuses correctly. Migrating real data is
  R6-MIGRATION-RECOVERY-01's outcome, not this one's, and this package does not
  claim production migration is complete.
