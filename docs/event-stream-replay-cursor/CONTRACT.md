# Durable capture-event replay cursor - J01 contract

Campaign: EVENT-STREAM-REPLAY-CURSOR-01 (J01 of J01-J04).
This document records the exact pins, the reproduced loss window, the owner trace at
the accepted base, and the settled J02/J03 design that this campaign will build.
Every pin below was re-verified against the trees on 2026-09-22.

## 1. Exact pins

| pin | value |
|---|---|
| charter | `docs/campaigns/EVENT-STREAM-REPLAY-CURSOR-01.md` in the TS tree at commit `3faabc106d3c83ec04bcd8c563b4883f4904aca0`, revision 9, blob `63cacb750a0205fe1e2a9bc911247c06503a1d6c` (supersedes revision 8 at `7b4b02929872dba0ea244c30ba3f210371b842ea`, blob `c6123dcebc54284eb2f78e74f83339c13471b3ea`) |
| accepted base | Rust `dev` = `0bacbff42f66f1a38f2215c02344ce7d23e9b2ff`, tree `80968d5ffcbc28577293c602d555279d11132fed` (canonical dev at the r9 composition release; supersedes `cf1719dd7d4d21d90d936848267eb9d0fe463b6b`, the r7-r8 base, which carries the LIVE-USAGE-ACCOUNTING-01 landing) |
| author root | branch `event-replay-composition`, clean tree at r9 merge candidate `6f93e74a865c20e4560f5b23eaee16c213286eb2`; the r5 root is preserved with `event-replay-sha2` at candidate `2d183d98e39e302bd6d8fe2eac648a528589955c` (tree `c32a90ee60a71c0157f84ff05d4a6ecde9ca418a`) and `event-replay-current` untouched at `c6f1a302708bf61061be600bea94c93a47fd913a` |
| law-pack | `fixtures/rust-reconstruction/law-pack.json` (TS tree), revision 9, blob `9d60bf2cb6f7ec08518167f87d0de6614de6dc3f` |
| spec | `specs/secode-rust-reconstruction-r1-2026-08-07.md`, blob `f3ea2932903e413068de0e2cc819db843b5050fd` (identical at the charter commit and at TS HEAD) |
| matrix | `docs/reports/SECODE_RUST_RATIFIED_ARCHITECTURE_SUPERSESSION_MATRIX_2026-08-08.md`, blob `745a981ab09001886ceb01d3fa88ede059512683` (identical at the charter commit and at TS HEAD) |
| alpha-0 | `docs/reports/SECODE_RUST_INTERNAL_ALPHA_0_CONTRACT_2026-08-20.md`, blob `acfb90e4364cbf918e857053d352f613e6c208f6` (sections consumed per the charter: Renderer transport, volatile-event reconciliation, acceptance-11) |
| health board | `docs/reports/SECODE_HEALTH_BOARD_2026-09-22.html`, blob `18291c55a0f084499d8fd5c3f48e2812fc402148` (diagnosis input, not authority) |
| frozen v2 OpenAPI | `FROZEN_OPENAPI_SHA256 = "0c3801f00ebc474222dd8bcf026aac5d34c82bf23ef24ac7111f5d776e758f4b"`, 75 schemas, 23,820 bytes, DTO version `grapt.api.dogfood0.dto.v1`; pinned at `crates/grapt-api/src/u3a/dto.rs:19`, fixture `fixtures/rust-reconstruction/compat/openapi/openapi.json` asserted by `crates/grapt-api/tests/u3a_typed_contract.rs` |

The spec and matrix blobs are byte-identical at the charter commit, at TS HEAD, and in the working
tree; nothing in the governing set moved between the charter and this start.

### Revision history (J01 start -> r9)

- r1-r4: J01-J03 built in root `rust-event-replay-r1` on branch `event-replay-cursor`
  at base `a03e494c`; J04 qualification was resource-blocked (charter revision 2-4 history).
- r5: composition admitted in the isolated root `rust-event-replay-r5`, branch
  `event-replay-current`, at the J01 base.
- r6: the `mutations/EVR/` owned-path deviation was admitted prospectively; the five r6
  review findings were corrected as one set (`e73d0483d`).
- r7: the canonical sha2 dependency was released and accepted (`cf1719dd`); the campaign
  re-pinned onto it on successor branch `event-replay-sha2` (`e75dea0e7`, `502ca36f2`); `event-replay-current` was frozen at
  `c6f1a302708bf61061be600bea94c93a47fd913a` and preserved.
- r8: the consolidated review HOLD on `d2143b640` was returned
  under the same owned outcome: B1 Windows-path/token codec, B2 durable-coordinate delivery
  across the live/replay join (the route splice), B3 an explicit usable outcome at the
  coordinate bound (the distinct count door plus the up-front width probe), B4 real product
  composition. `a8300604ce` resolved B1, B2, the EVR8 store side, S3 and S5; this increment
  closes B4 (the product journeys), the S1 documentation, the S2 unified runner, the S4
  census and the EVR6/EVR7 evidence rows.
- r9: the r8 B4 candidate `2d183d98` was released as a committed candidate, not a
  canonical landing (charter revision 9 at
  `3faabc106d3c83ec04bcd8c563b4883f4904aca0`); canonical dev had advanced to `0bacbff4`
  (LIVE-USAGE-ACCOUNTING-01). Composition: new standalone root
  `rust-event-replay-r9`, branch `event-replay-composition`, from exact `0bacbff4`,
  merging exact `2d183d98` with both parents (merge `6f93e74a`, tree `1c9744f3`). The single
  conflict (the census block in `cases.rs`) resolved by re-enumeration: the two advances are
  file-disjoint (LUA +9/+2/+11, EVR J01-B4 +12/+2/+18 over common ancestor `cf1719dd` at
  1_204/312/1_843), so the composition pins 1_225 files / 316 dirs / 1_872 entries with
  include topology unchanged (80/80/29); `store/lib.rs` auto-merged as a pure union. The r5
  root and `event-replay-sha2` are preserved untouched. No acceptance ID is completed by this
  amendment; historical receipts of `2d183d98` remain evidence only for their exact bytes.

## 2. The loss window (health-board diagnosis, reproduced at the base)

Diagnosis (health board, 2026-09-22, CRIT row; diagnosis input only, not authority):

> Event stream has no replay cursor by design; overflow disconnects - capture can drop turns.

Mechanism, traced to code at the accepted base:

1. A capture client connects to `GET /api/event` and receives a bounded queue of pending frames
   (`MAX_SSE_PENDING_EVENTS = 64`, `crates/grapt-api/src/u3a/sse.rs:31`).
2. When the queue overflows, the subscriber is REMOVED from the fan-out with a typed outcome
   (HTTP sink: one error frame then EOF, `sse.rs:391`, `sse.rs:539-543`; host sink:
   `Err(SseError::SubscriberOverflow)`). There is no replay: the hub "stores no replay cursor and
   no durable history" (`sse.rs:310-312`); overflow discards even the frames the receipts called delivered, and the typed refusal is sticky.
3. Events committed while the client is absent (overflowed, disconnected, or never connected) are
   delivered to zero queues. The commit-before-notice carrier
   (`crates/grapt-store/src/notify.rs:189`) proves the facts ARE committed durably; the
   advisory notice is identity-only and lossy by contract.
4. On reconnect the client receives only the `server.connected` frame and publishes from that
   moment on. The missed events are reachable only by durable re-list, never by the stream.

This is exactly the defect class named by the ratified law:

- spec, hard compatibility commitment 2: "Live delivery remains advisory; reconnect must
  converge from durable truth."
- spec, root correction 5: "explicit Session/Location interests, bounded queues, artifact
  references and cursor/re-list convergence replace global feed assumptions."
- matrix, Commit identity row: "aggregate order is monotonic-with-gaps and broad reads use
  interest-bound vector/page cursors."
- alpha-0 contract, acceptance 11: "Force an ordinary SSE gap, re-list to durable convergence,
  then stop the server, restart it, and replay the same session truth."
- alpha-0 contract, volatile event reconciliation: "SSE transports live updates but promises no
  replay cursor. Active-session and session-history/refresh reads restore durable truth after a
  gap, and tests force an event and terminal inside that gap."
- alpha-0 contract, renderer transport row: "Protocol-v2 `/api/event` is volatile SSE: data
  frames have no event id or replay cursor, and reconnect alone can miss events. ... reconcile
  disconnects through durable re-list/refresh reads; do not invent cursor semantics."

Reproduction (executable, at the accepted base, before any code change):

- test: `crates/grapt-api/tests/loss_window.rs` ::
  `a_lost_subscriber_never_receives_the_missed_events_through_the_stream`
- scenario: `fixtures/event-stream-replay-cursor/` (`manifest.json`, `actions.jsonl`,
  `expected/stream-observation.json`)

The test fills a bounded subscriber (pending=2) with two publishes, overflows it with a third,
commits two more events while it is absent, reconnects, and proves that no presented frame
carries a lost event, while the receipts prove the absent-window events reached zero queues. The host subscription shares the exact
fan-out and overflow policy the HTTP SSE body uses (`sse.rs:467-486`), so the proof holds for
`/api/event` itself.

## 3. Owner trace at the accepted base

### Store (grapt-store, grapt-event, grapt-types)

- Commit-before-notice carrier: `crates/grapt-store/src/notify.rs`. `CommittedNotices`
  (`notify.rs:189`) is the only carrier between a commit and a notification; its sole producer is
  the `Ok` arm of `GovernedTransaction::commit`; it is `#[must_use]` with `publish`/`discard`
  (`notify.rs:199`). A `DurableNotice` (`notify.rs:72`) carries identity only: event id,
  aggregate, sequence, kind, writer epoch. Per-transaction buffer bounded at 64
  (`notify.rs:65`); overflow drops and counts, never fails the append. The module doc names this
  campaign's surface explicitly: "A bounded lossy fanout with per-subscriber interests, queues
  and cursor convergence is the event-feed surface (`reconnect-feed`, paused behind this
  lane) and the R4 API." (`notify.rs:44-48`)
- Append chokepoint: `crates/grapt-store/src/write/event_log.rs:92`
  (`EventStore::append -> (Appended, CommittedNotices)`), in-transaction form at `:276`:
  fencing first, then idempotency check, then sequence allocation, then the projector loop.
  Schema: `event` rows carry an FK into `event_sequence (aggregate_family, aggregate_id)`
  (`crates/grapt-store/src/write/schema.rs:215`, `:245`); the sequence is the per-aggregate
  durable cursor source.
- Typed read law: `crates/grapt-event/src/legacy.rs`. `decode_for_read` (`:258`) yields
  `ReadOutcome::Decoded` or the classified `SkippedUnknownKind { seq }` (`:196`) - an unknown
  kind advances the cursor and is classified, silence is not an option; hard errors
  (`LegacyDecodeError`, `:144`: id/aggregate/negative-sequence/kind/oversized-data/data) refuse.
  `decode_for_replay` (`:204`) rejects unknown kinds outright. J02 serves rows through
  `decode_for_read`.
- Page-reader idiom to copy: `crates/grapt-store/src/mailbox.rs::read_actionable` (`:602`):
  fetch `limit + 1` so a filled page is not mistaken for the end; decode BEFORE any bound
  decision (a row that cannot be read refuses the page); the byte bound stops only a non-empty
  page (one oversize row cannot wedge); `next_after = Some(cursor) if more else None`. Bounds:
  `MAX_MAILBOX_PAGE_MESSAGES = 64`, `MAX_MAILBOX_PAGE_BYTES = 262_144` (`:84`, `:90`).
- Location interest map: `crates/grapt-store/src/session_list.rs` - the
  `session_list_projection` table (session id, project, workspace, directory, title, updated);
  transactionally projected with the append (may lag, never lead). The capture interest binds to
  it for the session family.
- Durable identity: `crates/grapt-types/src/event.rs` - `EventId` (`evt_*`, `:10`),
  `EventSeq` zero-based with `FIRST = 0` (`:16-19`), `WriterEpoch` with `FIRST = 1`
  (`:65-70`), `EventFencing { Fenced { epoch }, LegacyUnfenced }` (`:100`).
  `AggregateId = Session | Campaign | Form` (families "session" / "campaign" / "form",
  `grapt-event/src/envelope.rs`).
- Corrupt-row staging for witnesses (test-only): `crates/grapt-store/src/write.rs:509`
  (`hostile_rows::install_event`, `install_cursor`, ...).

### API (grapt-api)

- Hub: `crates/grapt-api/src/u3a/sse.rs`. `DogfoodEventHub` (`:312`), `try_new` (`:328`),
  `publish` / `publish_at` (`:337` / `:343`) through `publish_scoped` (`:354`): encode once,
  `interested` filter (`:488`), `offer` per subscriber; overflow removes the subscriber
  (`:391`). `invalidate()` (`:407`, projection failure -> error close) vs `reconcile()`
  (`:421`, terminal boundary -> drain valid frames then clean close). Host subscription
  `subscribe_host` (`:477`) shares the bounded queue and overflow policy with the HTTP `SseBody`
  (`:515`). Bounds: 256 subscribers / 64 pending / 262,144 event bytes / frame+32
  (`:28-31`).
- Route: `register_dogfood_event_route` (`sse.rs:590`) on `GET /api/event`: auth is the
  `SealedPrincipal` request extension (missing -> 401), interest is the frozen deep-object query
  grammar `EventLocationQuery` (`crates/grapt-api/src/u3a/query.rs:167`, parser `:176`):
  `Absent | Null | Value { directory, workspace }`.
- Event DTO: `crates/grapt-api/src/u3a/events.rs`. 32-variant closure (`:372`). Durable variants
  carry `DurableEventRef { aggregateID, seq, version }` (`:30-59`); the six kinds without a
  durable coordinate are `ServerConnected`, `ReasoningDelta`, `TextDelta`, `UsageUpdated`,
  `WebSearchUpdated`, `ToolProgress`.
- Composition: `DogfoodApplicationPort` (`crates/grapt-api/src/u3a/mod.rs:5`, application
  module) routes dispatch; the fake-port test idiom is `NeverApplication`
  (`u3a/routes_tests.rs:22-30`). Product wiring lives in `grapt-product` (not owned here), and
  `grapt-api` has NO `grapt-store` dependency: API-side tests use port doubles, never the real
  store.

## 4. Compatibility boundary: protocol v2 stays unchanged

- `GET /api/event` remains protocol-v2 volatile SSE: frames carry no SSE `id:`, the route accepts
  no `Last-Event-ID`, and there is no replay. Its OpenAPI bytes are frozen at
  `FROZEN_OPENAPI_SHA256` (section 1); 75 schemas, 23,820 bytes, 14 operations.
- Existing tests already pin this: `u3a_typed_contract.rs` (frozen OpenAPI), and the in-crate
  `sse.rs` tests `connection_frame_has_no_replay_id_or_retry_field` (asserts no `id:`/`retry:`
  across the 32-variant closure) and
  `a_slow_subscriber_overflows_and_fails_instead_of_replaying`. J04 re-runs them against the
  final composition and asserts the OpenAPI sha is unchanged.
- The new capture surface is ADDITIVE: a separate authenticated versioned route
  (`GET /api/capture/v1/events`), registered by a separate registration function. v2 bytes,
  route behavior, retained renderer behavior and the frozen OpenAPI document do not change. No
  cursor semantics are invented for v2 (alpha-0: "do not invent cursor semantics").

## 5. Machine-readable scenario

`fixtures/event-stream-replay-cursor/` records the J01 loss-window scenario (manifest, actions,
expected stream observation). The J03 delivery adds the end-to-end capture journeys (start from
cursor, crossover, overflow/reconnect, restart, location isolation, auth failure) to the same
corpus.

## 6. Cursor grammar (v1)

Scope: the session family only - the location binding exists only there. Campaign and form
families are out of scope; a generic feed framework is rejected by the charter.

- Interest: `CaptureInterest { All, Location { directory, workspace: Option<String> } }`,
  mirroring `EventLocationQuery` (`query.rs:167`): absent/null interest means all locations.
- Cursor token (v1): opaque base64url of canonical JSON:
  `{"version":1,"interest":<interest echo>,"coordinates":[[aggregateId, seq, writerEpoch], ...]}`
  - coordinates sorted by aggregate id and unique; writer epoch 0 means LegacyUnfenced;
  - an aggregate with NO coordinate is unconsumed and replays from `EventSeq::FIRST` (0);
  - bounds: at most 1024 coordinates, token at most 64 KiB;
  - NO wall time, NO rowid, NO global sequence - the cursor derives only from the ratified event
    v2 identity and monotonic-with-gaps aggregate sequences.
- Read contract (J02):
  `EventStore::read_capture_page(interest, after: &CaptureCursor, limits)
    -> Result<CapturePage, CaptureReadError>`
  - "strictly after": for aggregates with coordinates `seq > coordinate.seq`; for others, all
    committed rows; deterministic order `(aggregate_id, seq)`;
  - the continuation vector is built only from served rows (honest: the page says exactly what
    it gave);
  - rows serve through `decode_for_read`: decoded rows are served; `SkippedUnknownKind` is served
    as a typed skip row and its coordinate still advances; a hard decode error refuses the page
    (`CorruptRow`); an unreadable store is `UnreadableStore`;
  - ABSENT (store readable, zero matching committed events) is an empty page with no
    continuation. Absent, unreadable and zero remain distinct outcomes.
- Cursor verification, each a distinct typed refusal:
  `Malformed`, `WrongVersion`, `WrongLocation` (the bound interest differs from the requested
  one), `FutureCoordinate` (a coordinate exceeds the `event_sequence` head), `Stale` (the
  coordinate's aggregate has left the interest set).

## 7. Failure taxonomy

| outcome | when | evidence carried |
|---|---|---|
| absent | store readable, no committed events match the interest/cursor | empty page, no continuation |
| unreadable-store | the store cannot be read at all | typed store error, distinct from absent |
| malformed | the cursor token is not decodable v1 | typed refusal |
| wrong-version | decodable, but not cursor version 1 | typed refusal |
| wrong-location | the bound interest differs from the requested interest | typed refusal (409 at the API) |
| future-coordinate | a coordinate is beyond the aggregate's committed head | typed refusal (409) |
| stale | a coordinate names an aggregate that left the interest set | typed refusal (409) |
| corrupt-row | a committed row fails the hard decode law | page refuses; the row is never skipped |
| limit-exhausted | a page would exceed its row/byte bound | honest continuation cursor for the remainder |

## 8. Resource limits

| resource | bound |
|---|---|
| capture page rows | `MAX_CAPTURE_PAGE_ROWS = 64` (mirrors the mailbox idiom) |
| capture page bytes | `MAX_CAPTURE_PAGE_BYTES = 262_144` |
| cursor coordinates | 1024 |
| cursor token bytes | 64 KiB |
| capture stream subscribers | the existing `SseLimits` bound family (max 256) |
| capture stream pending frames | the existing `SseLimits` bound family (max 64) |
| frame bytes | the existing `MAX_SSE_FRAME_BYTES` bound family |
| replay loop | page-bounded: each iteration serves one bounded page and advances the cursor; it terminates on exhaustion, overflow or typed refusal - no unbounded catch-up loop, no polling |

## 9. J02 design (bounded durable page, grapt-store)

New file `crates/grapt-store/src/event_feed.rs` (re-exported from `lib.rs`); cursor types in
`crates/grapt-types/src/event.rs`. The read is the smallest synchronous page over committed
truth:

- session-family rows via the `event` table; the Location interest joins
  `session_list_projection` (directory + workspace match);
- cursor exclusion via a per-call temporary coordinate table with `NOT EXISTS`;
- `LIMIT rows + 1` for the honest continuation; decode before any bound decision;
- witnesses (`crates/grapt-store/tests/event_feed.rs`): positive multi-aggregate/location
  pagination with an exact multiset, no gap or duplicate, deterministic order; negatives: cursor
  row exclusion, wrong location/interest, malformed cursor, wrong-version cursor, future
  coordinate, corrupt row (via `hostile_rows`), unreadable store, limit exhaustion with a clean
  resume; restart durability: append, close, reopen, same page.
- resurrection mutants: EVR1 makes the boundary inclusive (`seq >=`) and the no-duplicate
  witness goes red; EVR2 skips an unreadable/over-budget row instead of refusing and the
  corrupt-row witness goes red.

## 10. J03 design (additive capture stream and convergence, grapt-api)

- New authenticated route `GET /api/capture/v1/events` (SSE), additive registration; v2 route
  untouched. Pre-stream typed refusals: 401 (no sealed principal), 400 (invalid query or
  malformed/wrong-version cursor), 409 (wrong-location, future-coordinate, stale cursor).
- On 200 the stream opens with a `capture.connected` frame carrying the protocol version, the
  interest echo and the initial cursor token - no clock, no minted random id.
- State machine in the stream body: REPLAY loops the port's `capture_page`, serving each row as
  a `capture.event` frame `{ coordinate: { aggregateID, seq, writerEpoch }, event: <DogfoodEvent
  JSON> }` with an opaque `capture.cursor` token frame every 64 presented rows and at the
  terminal; on exhaustion it crosses over to LIVE delivery as a NEW bounded hub sink variant
  (additive match arms; the v2 HTTP/host sinks are untouched): it presents only frames carrying a
  durable coordinate that match the interest and exceed the replay fence
  (`seq > fence[aggregate]`, fence = max presented seq per aggregate); transient frames without a
  durable coordinate are skipped. Tokens are minted from the fence.
- Exactly-once per coordinate: frames are at-least-once and every frame carries its coordinate,
  so a recorder deduplicates by coordinate.
- Terminals: `capture.terminated { reason: overflow | invalidated | reconciled | corrupt-row |
  unreadable-store, final cursor token }` then EOF. The new sink variant carries the terminal
  reason so invalidate (error close) and reconcile (drain then close) stay distinct.
- `CaptureApplicationPort` in `u3a/mod.rs`: a single `capture_page` method (the live side needs
  no port: hub frames already carry full `DogfoodEvent`s).
- Journeys (fake port doubles in `crates/grapt-api/tests/`): start from cursor, replay,
  crossover, overflow/disconnect, reconnect from the last acknowledged token with exactly-once
  per coordinate; restart (fresh hub, same durable data) preserves the same coordinate set;
  location isolation; auth failure.
- resurrection mutants: EVR3 removes the replay fence and the exactly-once journey goes red;
  EVR4 removes the location binding from the replay read and the location-isolation journey goes
  red.

### 10a. As-built amendments (J02-J04 and r8, against the design text above)

- The live phase presents NO raw live frames: the durable page is the sole presentation
  source, and a live frame is only a wake that re-enters the replay from the fence. This is
  stronger than the design text: a committed row with no v2 frame still reaches the reader
  exactly once, because the owed truth is read from the store, never from the hub's bytes.
- The wake law: an unscoped frame is wake material for EVERY capture interest; a frame
  scoped to another location never wakes the sink. The hub filter is queue protection only -
  the location filter is applied authoritatively by the durable page's SQL, and the fence
  advances only on rows the page presents, so an admitted frame cannot poison it. The r8 B4
  product journey proved the strict-filter form starves a real location stream, because the
  product's session publish paths are unscoped.
- Pre-stream width probe: before the sink exists, the route counts the interest's DISTINCT
  aggregates (`count_capture_aggregates`; a missing projection table is absent, and absent is
  zero, never unreadable) and refuses up front with 409 "capture interest exceeds the cursor
  bound" when the width exceeds the 1024-coordinate bound - a usable outcome instead of a
  stream that dies mid-catch-up.
- Terminals (eight): `overflow | invalidated | reconciled | corrupt-row | unreadable-store |
  coordinate-bound | frame-bound | cursor-refused`, each with the minted resume token, then
  EOF. `coordinate-bound` names the 1024-coordinate wall hit mid-stream; `frame-bound` means
  one row did not fit one SSE frame WITHOUT moving the fence, so a reconnect re-lists it;
  `cursor-refused` is a mid-stream page refusal (the cursor outlived its truth, or the bound
  was exceeded under the stream).
- The B4 product projection layer (`grapt-product`) extends the store's `SkippedUnknownKind`
  law from unknown kinds to KNOWN-but-unprojectable kinds: a stored kind whose durable
  payload does not carry what the wire form needs is named, skipped, and the fence still
  advances over it; a present-but-disagreeing sessionID or an undecodable identity is a
  CorruptRow refusal, while an absent sessionID is not corruption.

## 11. Mutation prefix and census

- Prefix: `EVR`. Census at the accepted base (2026-09-22): whole-token `rg -w EVR` over the
  entire author root returns zero matches; the per-crate manifests
  (`crates/*/mutations/manifest.json`, 25 crates) contain no `EVR` token. The census is recorded
  in each `mutations/EVR/manifest.json` produced under J02/J03. The r8 B4 increment was measured
  against that census on `a8300604ce`: files 1_214 -> 1_216, entries 1_859 -> 1_861, no include
  site added or removed (80 sites, 80 targets, 29 carriers); the row is pinned in
  `crates/grapt-types/tests/direct_include_format/cases.rs`.
The r9 composition was measured at merge `6f93e74a` (base `0bacbff4`): 1_225 files / 316
dirs / 1_872 entries (the file-disjoint union of LUA +9/+2/+11 and EVR +12/+2/+18 over the
common ancestor `cf1719dd`); no include site added or removed (80 sites, 80 targets, 29
carriers); the row is pinned in the same cases.rs at 999 lines.
- Convention: per-crate `mutations/EVR/manifest.json` + `run.py`; rows `{ id, law, file, old,
  new, test_target, test }`; each mutant runs in its own disposable worktree at the candidate
  commit; the runner matches `EVR[0-9]+` on whole tokens. Mutants are proven red-then-green;
  attribution runs use `cargo test --no-fail-fast`.

## 12. Journey map and terminal condition

| journey | deliverable | gates |
|---|---|---|
| J01 (this delivery) | CONTRACT.md, `fixtures/event-stream-replay-cursor/`, `crates/grapt-api/tests/loss_window.rs` | `cargo fmt -p grapt-api --check`, `cargo clippy -p grapt-api --all-targets -- -D warnings`, `cargo test -p grapt-api` |
| J02 | `event_feed.rs`, cursor types, store witnesses, mutants EVR1/EVR2 | per-crate gates for grapt-store, grapt-types, grapt-api |
| J03 (+r8) | capture route + stream, `CaptureApplicationPort`, journeys, mutants EVR1-EVR9 across the two per-crate manifests (store: EVR1/EVR2/EVR4/EVR7, api: EVR3/EVR5/EVR6/EVR8/EVR9) | per-crate gates for grapt-api, grapt-store, grapt-types, grapt-product |
| J04 | RESULTS.md, `scripts/qualification/event-stream-replay-cursor/`, `remote-compute/event-stream-replay-cursor.json`, final composition battery + both ambient negative lint controls on the allocated Linux carrier, independent cold review, landing, post-land reproduction, release-graph projection | full gate once after the final executable change |

Not delivery (charter): archaeology only, a store-only page, an API-only happy path, or a green
test without both cursor mutants. Terminal means the accepted candidate is landed, reproduced
and projected; the campaign is cancelled; or all independent work is exhausted behind a concrete
external blocker.