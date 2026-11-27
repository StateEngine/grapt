# Vendored `regress` 0.10.5, with a work-budget patch

## Why this is vendored rather than depended on

`secode-form` evaluates a form field's `pattern` constraint, and the frozen
contract for that constraint is ECMAScript `new RegExp(pattern).test(value)` with
no flags. `regress` is the engine that implements those semantics; a general Rust
regex engine measurably does not (identity escapes, ASCII-only classes,
`[]`/`[^]`, and UTF-16 code-unit rather than scalar-value matching).

Upstream exposes no evaluator work budget. Under this project's
bounded-everything law an unbounded backtracker is not admissible in a request
path, and a wall-clock timeout is not a bound: a runaway match cannot be stopped
from outside the engine, so a timeout observes an overrun without ending it.

This vendored copy carries the smallest patch that exposes a **deterministic
work budget** with a **distinct exhausted result**, so exhaustion is never
translated into a match or a non-match.

## Exact provenance

| field | value |
|---|---|
| crate | `regress` |
| version | `0.10.5` |
| source | `registry+https://github.com/rust-lang/crates.io-index` |
| crates.io checksum (sha256 of the `.crate`) | `2057b2325e68a893284d1538021ab90279adac1139957ca2a74426c6f118fb48` |
| upstream author | ridiculous_fish |
| license | MIT OR Apache-2.0 (both texts retained verbatim in this directory) |

The checksum above is the one Cargo recorded in this workspace's `Cargo.lock`
for the registry artifact this tree was copied from. `LICENSE-MIT` and
`LICENSE-APACHE` are unmodified.

## What was changed

Every change is confined to adding a budget; no matching semantics are altered.
Within budget, observable match and error behaviour is byte-for-byte the
upstream engine's, which is what the differential test against the oracle
asserts.

See `PATCH.md` in this directory for the exact change list.

## Not a fork

This is a pinned vendor of one released version plus a focused correction. It is
not a maintained fork: the intent is to carry the budget upstream, and if
upstream gains an equivalent budget this directory should be deleted and the
registry dependency restored.
