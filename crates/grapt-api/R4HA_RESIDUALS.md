# R4H-A residuals

Lane: R4H-A (`grapt-api`, transport trust boundary).
Author: the author (`agt-demo`).
Scope: bounded API routes and closed authentication.
Base: canonical `364c23d0a37406205d04b18d0e98304110a477fa`, tree
`ca09ad4791428bcc0e45b769aa43e05474296422`.
Preflight: `C:\fixtures\R4HA_SEALED_PRINCIPAL_ROUTER_TAKEOVER_PREFLIGHT_2026-08-13.md`
(sha256 `f8a1f5c9da8f935ecab328ced3ecde0be3e19f6b44ad998440340e495e418d2e`).

Everything below is a residual, a disclosed deviation, or a correction to the
preflight this lane was specified from. Nothing here is a request to relax a
ruling.

---

## 1. Owed to another lane

### 1.1 The DG follow-on (R4HA-07) — nobody owns it yet

`mutations/dg/manifest.json` records its own limitation verbatim: DG01-DG09 and
DG16-DG24 smuggle real workspace crates rather than the literal `grapt-api`
named at master spec L1329, because Cargo must RESOLVE every dependency in a
real manifest and a row naming an absent crate is a compile kill rather than a
witness. The literal name is witnessed only in-test, by the
`the_gate_catches_every_back_edge_form` case set in nine crates.

**The moment this lane lands, that limitation dissolves**: `grapt-api`
resolves, and a real-manifest back-edge mutation naming it becomes constructible
for the first time in all eighteen crates.

R4HA-07 rules that this is a separate increment, required after A exists and
before A's canonical landing, and that an independent
author/reviewer is assigned. It does not block A's authoring or cold review. It is named
here so the DG limitation note cannot go stale unnoticed.

### 1.2 The positive authorization path cannot be witnessed in this lane

This is the largest coverage statement in the lane and it is a consequence of
R4HA-04 rather than a gap in the work.

`authorization::authorize` refuses without a `SeatCapabilityProof`. A proof
borrows a real `grapt_types::CapabilityGrant`, whose only constructor is
`CapabilityGrant::from_live(&LivePermissionDecision)` — and
`LivePermissionDecision::mint` is `pub(crate)` inside `grapt-types`, whose test
fixture route is `#[cfg(test)]`. **No crate in this workspace can obtain a
`CapabilityGrant`, including this one and including its tests.**

That is the ruling working as written: production A authenticates and refuses,
and an allow requires a proof supplied through the later composition seam. The
cost, stated plainly:

| Unwitnessed | Where |
|---|---|
| `AuthorizationDecision::Allowed` is ever returned | `authorization.rs`, final line of `authorize` |
| a proof issued for seat B does not authorize seat A | the `proof.recipient() != principal.recipient()` branch |
| a proof that does not cover the capability/scope is refused | the `!proof.authorizes(..)` branch |
| `govern` performs EXACTLY ONE invocation on an allow | the `is_allowed()` arm |

Every refusal path, including refuse-before-effect with zero invocations, IS
witnessed. The four rows above belong to the composition lane's evidence, and
that lane must not be reviewed as if A had already covered them.

The alternative — a public trait a dependent could implement to return an allow,
or a `#[cfg(test)]` proof constructor — was rejected. Both are a second public
route to authority wearing a different name, which is the exact defect
`grapt-types::authority` documents.

---

## 2. Disclosed deviations from the preflight's proposed shape

### 2.1 Sealed-path witnesses live in `src/witness.rs`, not `tests/`

Packet §11 placed `auth_contract.rs`, `directory_contract.rs`,
`authorization_contract.rs` and `takeover_contract.rs` under `tests/`. They
cannot live there.

Every one of those laws starts from a sealed value, and the fixture route to a
`VerifiedSubject` is `#[cfg(test)]` — which compiles into the library's own test
binary and nowhere else. An integration test under `tests/` is a **separate
crate** that links this one as a dependency, so it cannot reach the fixture
route either. That is the seal working, not a hole in it.

So the sealed-path witnesses are `src/witness.rs` (`#[cfg(test)]`, 19 tests),
and `tests/` carries what genuinely needs only the public surface:
`router_contract.rs`, `dto_error_contract.rs`, `acceptance_corpus.rs`, the two
dependency gates and `compile_fail.rs`. The corpus manifest names `lib` as the
owning target for the eleven scenarios that live there, and
`tests/acceptance_corpus.rs` binds them by embedding `../src/witness.rs` with
`include_str!` and walking `#[test]` attributes.

### 2.2 Two probes substituted

* **"the principal is not `Deserialize`"** has no subject. This crate takes no
  `serde` dependency in any table, so there is no trait in scope to fail
  against. The property is held structurally instead, by
  `tests/allowed_external_dependencies.rs`, which turns red if `serde` is added
  to any table.
* **"registration after seal is refused"** was replaced by
  `06_sealed_router_admits_no_further_route`, which witnesses a stronger
  property: `seal` takes `self` by value, so this is a move-checker error rather
  than a runtime refusal. `ApiRefusal` therefore carries **no** `RouterSealed`
  variant — a refusal that can never be constructed is a vocabulary entry that
  lies about what can happen.

### 2.3 Two admitted dependencies deliberately not taken

Packet §12 and R4HA-09 admit them; permission to depend is not an instruction to
add an edge.

* **`tokio`** — admitted for "bounded synchronization only … no `time` unless a
  named use exists". No named use exists. This crate is synchronous domain law
  and `RecipientConnectionRegistry` is owned behind a single `&mut`, so no
  synchronization primitive is reached for.
* **`grapt-location`** — A opens no socket and names no address. The host lane
  that consumes a location type adds the edge.

`tests/dependency_direction.rs` therefore carries the exact current allowlist
`["grapt-types"]` rather than L247's permitted superset, so widening is a
reviewed edit here rather than silent `Cargo.toml` drift.

### 2.4 Lease release is explicit, not `Drop`-based

Packet §9 says a lease "is consumed or dropped, and dropping releases the slot".
A `Drop`-based release needs the lease to hold a back-reference to the registry,
which means interior mutability and a registry two owners can observe at once.
`RecipientConnectionRegistry::release` CONSUMES the lease instead, keeping the
registry behind one `&mut` and the bound observable from one place. The lease is
still non-clone, so it cannot be released twice, and a displaced lease is
refused rather than allowed to evict the holder that displaced it.

### 2.5 The directory generation is directory-wide, and that is coarse

Revoking one subject advances the generation for all of them, so every live
principal becomes inert and every connection must re-authenticate. Deliberate: a
per-subject generation would require consulting the directory per subject on
every authorization to know which generation is current — the same lookup, with
a wider window in which a half-revoked directory answers inconsistently. Fail
closed and coarse beats fail open and precise. Recorded because a later lane
measuring reconnect storms will meet it.

---

## 3. Corrections to the preflight this lane was specified from

The preflight is this lane's own prior work. Three of its measurements were
wrong or stale and are corrected here rather than carried forward.

### 3.1 The whole-token census is 447 rows, not 448 — and `A` is 22, not 23

R4HA-02 required a final fail-closed whole-tree inventory at the exact base
before `QA01`. Re-earned at `364c23d0` by walking the tree (the walk must match a
`mutations` path SEGMENT, not a leaf directory name, or it misses
`mutations/dg` and `mutations/f4a` — the first attempt at this census made
exactly that mistake and reported 8 manifests instead of 10):

* **10** manifests, **447** total rows = **441** conforming whole tokens + **6**
  non-conforming (`MH1A`, `MH1B`, `MH2A`, `MH2B`, `MH2C`, `M06B`).
* Per-manifest: artifact 22, campaign 14, event 29, provider 19, runtime 43,
  schema 67, store 153, types 8, `mutations/dg` 33, `mutations/f4a` 59.
* The preflight's table said 448 and `A` = 23. Measured now: `A` = **22**, and
  the preflight's own `M` attribution (event 7) was one high while its `M` total
  (116) was right. The corrected total is 447.
* **`QA` is absent as a whole token from every manifest**, which is what the
  allocation needed. `Q` = 12 (`grapt-runtime`), `A` = 22
  (`grapt-artifact`); both prefix-of collisions are disclosed in the manifest's
  `prefix_note` and witnessed in both directions by
  `mutations/test_prefix_selection.py`.

### 3.2 The resolved `axum` is 0.8.9, not 0.8.8

The preflight cited 0.8.8 because that was the newest version cached locally;
the resolver chose **0.8.9**. `src/routing/path_router.rs` is byte-identical
between the two and both pin `matchit =0.8.4`, so the measured panic set
carries — but `src/router.rs` names the resolved version, and the manifest pins
`axum = "=0.8.9"` exactly, because the panic set is part of this crate's
correctness argument and a patch bump must be a reviewed edit that re-measures
it.

### 3.3 The lock delta, which the preflight left unmeasured

Packet §3.3 item 3 explicitly deferred this. Measured at the candidate:

* `Cargo.lock` goes **197 → 202** `[[package]]` entries.
* Added: `axum`, `axum-core`, `matchit`, `mime`, and `grapt-api` itself — four
  new third-party packages.
* **The already-pinned HTTP stack is undisturbed.** `hyper 1.11.0`,
  `hyper-util 0.1.20`, `tower 0.5.3`, `tower-http 0.6.11`, `http 1.5.0`,
  `http-body 1.1.0`, `http-body-util 0.1.5`, `bytes 1.12.1`, `tokio 1.53.1` and
  `reqwest 0.13.4` are byte-identical before and after.

---

## 4. Corpus custody

A owns `fixtures/rust-reconstruction/r4-api-auth-foundation/` under contract
`grapt-api-auth-acceptance-corpus/v1` (R4HA-08). R4H-L's
`r4-host-auth-foundation/` was neither read into nor written; its validator
admits exactly four scenarios in order under
`grapt-location-acceptance-corpus/v1`, so adding a row there would have turned
that lane red.

One custody note for a reviewer: the corpus is resolved from
`CARGO_MANIFEST_DIR` walked to the repository root, **not** through
`test-support/fixtures.rs`. That helper defaults to
`C:/workspace/repo/fixtures/rust-reconstruction`, which is the FROZEN oracle
corpus in a different repository; this lane's corpus is authored in the Rust
tree and travels with the candidate. The R4H-L candidate resolves its own corpus
the same way.

---

## 5. Not in scope, and not attempted

No socket, address, port, listener or bind plan (L). No accept loop, wire limit,
HTTP status, drain, close or readiness receipt (H). No product endpoint,
including `/health`. No durable row, event, store transaction, mailbox,
permission or capability settlement. No credential backend, password, token,
OAuth, Basic auth or TLS. No generated client, OpenAPI or TypeScript. No service
registration, PID/URL discovery or candidate switch. Nothing in this crate
persists: no lease, takeover, directory entry or principal survives the process.
