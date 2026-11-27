# TOOLCHAIN-ACTIVATION-01 contract

The verified Rust tool-binary activation boundary. This document is the J01
witness: the complete typed contract, the bounds it declares, the refusal
taxonomy it owes, and the executable counterexample proving the accepted base
does not already satisfy the law.

## Authority and consumed law

- Charter: `docs/campaigns/TOOLCHAIN-ACTIVATION-01.md` revision 1 (controller
  blob `1bebe381bb71a02f9035afcc42872173afb92895`).
- Accepted base: `6fe79ddf09272aab309d5ce14276af8db3c15da8` on branch
  `dev`, branch `toolchain-activation`.
- Consumed law row, exactly one: section 8 row 1 of
  `SECODE_RUST_RECONSTRUCTION_DISPOSITION_2026-08-07.md`
  (sha256 `cca761d01667451dbac9bda6a8c6424ce6c25313fdc4457882f41fc10b2a70fc`,
  re-verified at delivery): a tool binary activates only when its archive
  sha256, source URL and per-target pin all match; a revoked or re-cut pin at
  the same version refuses; identity write failure rolls back activation.
  The disposition names the Rust owner `grapt-toolchain`.
- Law pack: `fixtures/rust-reconstruction/law-pack.json` revision 9 (source
  pins only; no additional law rows are consumed).
- Observable compatibility evidence, not ported: the frozen-oracle witness
  pair `packages/core/src/ripgrep/acquire.ts` and
  `packages/core/test/ripgrep-acquire.test.ts` at oracle `42e4888cc0`
  (19/0, mutation-proven including `identity-write-failed` rollback). What is
  owed from it is the OBSERVABLE behavior (pin table discipline, identity
  match law, staged activation, rollback), not its Effect graph.

## The obligation and its base state

S1 (the tool-acquisition row of the security-continuity disposition) has no
Rust owner at the accepted base. The executable counterexample
(`scripts/qualification/toolchain-activation/survey_base.py`, receipt at
`fixtures/toolchain-activation/receipts/base-6fe79ddf-counterexample.json`)
proves, through the intended product seam:

1. The workspace has no `grapt-toolchain` member.
2. The product binary `opencode-cli` exposes no toolchain activation verb:
   its command surface is `--version | startup | serve | handover` plus the
   private codemode worker argument, so `toolchain activate` through the
   seam is an unknown-command refusal (exit 2).
3. The one tool binary the Rust package ships today (plugin-runtime Node) is
   guarded at EXECUTION time by the `grapt-plugin` supervisor (byte digest
   plus version check before spawn), but its ACTIVATION has no archive
   digest verification, no same-version re-cut refusal, and no
   identity-write rollback. Operator-supplied shell tools are never
   activated by any product path at all.

A green happy path without these named refusals does not release the lane:
the refusal taxonomy below is the contract, and each row is witnessed.

## Typed contract

The machine-readable form is `fixtures/toolchain-activation/contract.json`
(schema `grapt.toolchain-activation.contract.v1`); this section states the
law it encodes.

### Values

- `ToolName` — 1..=64 of `[a-z0-9._-]`, leading byte `[a-z0-9]`, and never a
  Windows reserved device name. The name is the FIRST layout segment of the
  activation tree: a segment the host treats as a device is not a name. The
  identity record and the pin must agree on it.
- `ToolVersion` — 1..=32 printable ASCII. An equal version with a changed
  archive digest or source is a RE-CUT, not an upgrade, and refuses reuse.
- `TargetTriple` — 1..=128 of `[a-z0-9._-]`. The per-target pin key. An
  absent entry is `target-unsupported`; there is no default target.
- `SourceUrl` — 1..=1024 bytes, no control bytes. The exact position of the
  archive relative to the activation root. The shipped deterministic adapter
  admits scheme-less relative paths only: any scheme prefix, including
  `file:`, is refused `source-unsupported` (R11), and a network source is an
  explicit release-input boundary, not a core capability (no network call
  from core logic).
- `Sha256` — exactly 64 lowercase hex characters. The archive digest and the
  binary digest are separate facts; the identity carries both.
- `ArchiveFormat` — closed vocabulary: `tar` (uncompressed ustar), a closed
  TYPED value. The shipped bounded decoder admits no other format; because
  the vocabulary is closed at the type, a pin declaring one is refused
  `pin-malformed` (R06) at pin decode — a runtime `format-unsupported`
  refusal is unreachable, which is why contract revision 2 removed R12 from
  the matrix.
- `EntryName` — 1..=256 bytes, relative, no dot segments, no absolute path,
  final segment not a Windows reserved device name: the pinned location of the
  binary inside the archive. The final segment is where the executable lands
  on disk, and a device name there is not a file.
- `InstalledIdentity` — schema `grapt.toolchain.identity.v1`, exactly nine
  fields: `tool`, `version`, `platform`, `arch`, `target`, `source`,
  `archive_sha256`, `binary_sha256`, `installed_at_ms` (injected clock,
  milliseconds since epoch). Presence of a binary is NOT evidence of
  provenance; only a record that matches the CURRENT pin vouches for it.

### The identity match law

An existing activation is reusable only when ALL of these hold:

1. the identity record decodes strictly (every field present, bounded,
   well-formed; a missing or foreign field is an undecodable record, and an
   undecodable record is never a vouch);
2. `tool`, `version`, `platform`, `arch`, `target`, `source` AND
   `archive_sha256` all equal the current pin;
3. the canonical executable bytes on disk hash to `identity.binary_sha256`.

Version and target alone never match. Revoking a compromised digest, or
upstream re-cutting a release, changes the pin while leaving version and
target identical; a record written under the old pin must not keep the old
binary alive, and the activation re-acquires under the new pin. If the
revocation removes the target from the table entirely, the refusal is
`target-unsupported` and the unvouched binary is never executed.

### Bounds

| Fact | Bound |
| --- | --- |
| pin file | 64 KiB, schema `secode.toolchain.pins.v1`, at most 16 entries |
| identity record | 4 KiB, schema `grapt.toolchain.identity.v1` |
| archive bytes | 8 MiB — the single bound: the archive decodes in memory and is never staged as a file, so the verified binary is bounded by the same 8 MiB |
| tar entries | 16 |
| tar path depth | 4 |
| tar entry name | 256 bytes |
| receipt | 16 KiB |

Every bound is enforced before the bytes are trusted, and an overrun is a
named refusal, never a silent truncation.

### Staged layout (relative names, stable)

```
tools/<tool>/bin/<exe>                    canonical executable
tools/<tool>/bin/<exe>.staged             staged copy, re-hashed before publish
tools/<tool>/bin/<exe>.last-known-good    retained previous executable
tools/<tool>/identity.json                identity record (staged + renamed)
tools/<tool>/.staging/                    private bounded staging root
tools/<tool>/.activation.lock             exclusive serialization lock
```

The archive and its unpacked tree never live beside the canonical
executable, and staging is removed however the attempt ends. Staging state
left by a crashed attempt is UNTRUSTED: it is cleared on the next attempt,
and the canonical binary plus its record are never touched by staging.

### Refusal taxonomy

Twenty-one typed reasons, `R01`..`R22` minus the unreachable `R12`, in the
machine-readable matrix (revision 3). The distinctions the law demands:

- **Unsupported target** (`R10`) is a pin-CONTENTS fact: the well-formed pin
  table was read and has no entry for the requested target.
- **Non-Git or irrelevant workspace state** (`R07` not a Git worktree, or the
  attribution plumbing cannot be believed, `R08` untracked at HEAD, `R09`
  on-disk bytes differ from the HEAD blob) is a pin-SOURCE attribution fact:
  an independently reviewed pin is a COMMITTED artifact, and a pin that
  cannot be attributed to a commit is refused as a workspace-state problem,
  never silently read.
- **Malformed archive** (`R22`) is a header or entry the decoder cannot
  verify or parse — an unverified checksum, a non-octal field, foreign
  magic: **truncated stream** (`R17`) is a framing fact — an empty or
  cut stream, data after the terminator, non-zero padding, an overrun.
  A header that does not verify is malformed; a stream that runs out is
  truncated.
- **Missing or unreadable pin data** (`R03` missing, `R04` unreadable,
  `R05` oversized, `R06` malformed) is a pin-DATA fact about the file
  itself.

No two of these collapse into one error. Every diagnostic names the failed
tool, version, target and pin digest without leaking filesystem paths or
secrets.

### Activation algorithm (law, not implementation)

1. Resolve the root (`R01`/`R02`), then the pin (`R03`..`R09`), then the
   target entry (`R10`).
2. Take the exclusive lock (`R18` on contention; concurrent callers
   serialize, never race, and no partial bytes are exposed).
3. Clear untrusted staging.
4. Verify the existing activation against the CURRENT pin (match law). A
   match reuses it: no fetch, no write.
5. Fetch the pinned archive through the deterministic adapter (`R11` on a
   source it does not admit, `R13` on a delivery failure), verify its digest
   (`R14`), and decode it IN MEMORY under the bounds (`R15`/`R16`/`R17`/`R22`):
   the archive itself never stands as a file.
6. Stage the binary beside the canonical name, re-read and re-hash it
   (`R19`); a copy that reports success but lands short is incomplete.
7. Retain the previous executable as a hard link to last-known-good, then ONE
   rename publishes staged to canonical (`R20` on failure: the canonical path
   is untouched, the previous executable remains when there was one, and the
   staged file is removed). A crash anywhere in the publish window leaves the
   previous executable or the new one at the canonical path, never neither.
8. Write the identity record staged and renamed (`R21` on failure): remove
   the new binary, restore the previous executable and its record (or leave
   nothing when there was no previous). An unattributable executable is
   never left at the canonical path, and a run that cannot record its
   identity does not get to execute it either.
9. Release the lock, remove staging, and emit the bounded receipt.

## Revision history

- **Revision 1** (J01): the matrix as first authored — twenty-one reasons
  including `R12` (`format-unsupported`), with the adapter described as
  admitting the `file:` scheme only.
- **Revision 2** (J02): two amendments aligning the matrix with behavior the
  code already proved. (a) `R12` removed: the format vocabulary is a closed
  typed value, so a foreign format string is a malformed pin refused `R06`
  at pin decode — an unreachable row is not a refusal the boundary owes. The
  matrix now carries the twenty reasons `R01`..`R21` minus `R12`, the set
  the code's taxonomy counts. (b) `SourceUrl` corrected: the shipped
  deterministic adapter admits scheme-less relative paths only; any scheme
  prefix, including `file:`, is refused `source-unsupported` (R11).
  Revision 1's `file:` wording described an adapter the code never shipped.
- **Revision 3** (J02, review corrections on the composed candidate):
  (a) `R22` (`archive-malformed`) added and `R17` reframed to framing facts
  only — a header the decoder cannot verify or parse is malformed, a short
  or cut stream is truncation; the matrix now carries the twenty-one reasons
  `R01`..`R22` minus `R12`. (b) The binary bound unified into the single
  8 MiB archive bound: the archive decodes in memory and is never staged as
  a file, so a separate binary staging bound was a second owner of one
  fact. (c) The publish law restated: hard-link retain, then ONE
  replace-existing rename; a crash in the window leaves the previous or the
  new executable, never neither (`R20` reframed). (d) `ToolName` and
  `EntryName` gain the layout-segment law: a segment a host filesystem
  treats as a device is not a name. (e) The diagnostic context gains the pin
  source and archive digest for every refusal after pin decode. (f) The
  product seam admits a target only when it describes this host: the seam
  activates the host's own tool tree.

## Journey coverage

- J01 (this document): the matrix above, plus the executable base
  counterexample and its receipt.
- J02: the `grapt-toolchain` owner plus the `opencode-cli toolchain
  activate` product seam; corpus actions cover install, reuse, upgrade,
  revocation, rollback, the crash-window and concurrency witnesses, and every
  named refusal; the seam itself is additionally witnessed by the cargo
  integration tests in `crates/grapt-product/tests/toolchain_activate.rs`
  (host-only target, seam refusals); the resurrection mutants are the
  `TMA1xx` rows in `fixtures/toolchain-activation/mutations.json`.
- J03: qualification corpus and receipts, the supported-target/pin matrix,
  recovery and migration guidance, the Linux battery, and TM01 (an
  independent manual witness of install/reuse/revocation/rollback through
  the real product seam on a disposable root).