# Accepted deviations from the frozen compatibility record

`openapi.json` in this directory is the frozen OpenCode mirror and stays byte-for-byte
unchanged (`FROZEN_OPENAPI_SHA256`, asserted by `crates/grapt-api/tests/u3a_typed_contract.rs`).
This file lists the only places where frames on the live wire knowingly differ from it, why, and
under which condition. A deviation not listed here is a defect.

## Elided `session.text.ended` and `session.reasoning.ended` on overflow

- **Ruling:** accepted deviation, Tier 2.
- **Kinds:** `session.text.ended`, `session.reasoning.ended` (both carry `SliceEndedData`).
- **Condition, overflow only:** the frame carrying the block's whole text fails to encode within
  the live event bound (`MAX_SSE_EVENT_BYTES`, `SseError::EventBytes`). Every frame that fits is
  sent exactly as the frozen record describes, byte for byte.
- **Shape when elided:** `data.text` is `""`, and `data` adds two fields the frozen record does not
  declare (it has `additionalProperties: false`):
  - `textBytes`: the block's raw text length in bytes;
  - `textSha256`: the lowercase hex SHA-256 of the block's raw text.
- **Why:** a block may hold up to `PUBLIC_CONTENT_BYTES` (256 KiB) of text, and the ended frame
  re-carried all of it. With JSON escaping and the envelope, it could not fit the 256 KiB event
  bound, so the publication was refused and every live stream ended mid-turn (the at-bound ACP
  refusal). The deltas have already delivered that text. The elided frame closes the block with
  what a client needs to check its accumulation: on a mismatch it hydrates from durable history,
  as it would have anyway. The frame appears only where the stream used to end, so it is no
  worse for a strict client and strictly better for every tolerant one. No in-repo reader
  (`grapt-cli` ACP and events, `grapt-tui`) reads `ended.text`.
- **Durable truth:** unchanged. The `session.content.appended` fact and its artifact keep the
  full text; nothing new is persisted, so a revert needs no migration.
- **Where:** `crates/grapt-product/src/dogfood/execution/block_events.rs` (`ended_elided`),
  `live_sink.rs` (`publish_capture`), `crates/grapt-api/src/u3a/events.rs`
  (`SliceEndedData::elided`).

## `Session.Info` without `tokens` when lifetime metrics exceed their bound

- **Ruling:** a metrics shortfall never fails session list, get, create or fork.
  The exact response contract below applies to
  all six routes that encode a Session row (list, get, create, fork, and the plugin host's get
  and create) return it, and 400 is reserved for corruption.
- **Schema:** `Session.Info` (closed, `additionalProperties: false`, `tokens` required).
- **Condition, bound only:** the Session's lifetime metrics read (`SessionHistoryStore::fold_bounded`
  at the accounting bounds, 65,536 rows / 64 MiB) refuses with `WindowTooLong` or
  `WindowDataTooLong`. A corrupt row is still a refusal, never this shape. Every Session within
  the bound is encoded exactly as the frozen record describes, byte for byte.
- **Shape when over the bound:**
  - `tokens` is absent, never zeros and never a truncated total;
  - `metrics` is added: `{"state":"exceeds-bound","rows":<row bound>,"bytes":<byte bound>}`;
  - `time.updated` is the commit time of the Session's newest visible durable event, from one
    indexed read. Within the bound it is the newest message commit, so this value is never
    earlier than the one the fold would give.
- **Why:** every Session row folded its whole history to total its tokens. Past the transcript
  fold's 5,000 rows that refused, so one long Session made `session.list` answer 400 (no TUI could
  start on that service) and `session.get` answer 400 (it could not be reopened). The bounded read
  moves that point to a producer day; this deviation makes sure that a longer Session still has
  a row. No in-repo reader (`grapt-tui`, `grapt-cli`) reads `Session.Info.tokens`.
- **Durable truth:** unchanged. Nothing new is persisted.
- **Where:** `crates/grapt-product/src/dogfood/mod.rs` (`session_metrics`, `session_info`),
  `crates/grapt-api/src/u3a/vocabulary/session.rs` (`SessionInfoDtoMetrics`),
  `crates/grapt-session/src/history/store.rs` (`fold_bounded`, `newest_commit_ms`).

## SC-LIST-01 — unreadable session metadata preserves the indexed row

- **Scenario:** a session's index entry is readable while its lifecycle metadata is corrupt
  or unavailable. The list retains its identity, title, placement and update time.
- **Shape:** `Session.Info.metadataStatus` is `quarantined` for corrupt metadata or
  `unavailable` for a storage refusal. `time.created` is absent when creation is unknown;
  configuration, usage and cost are also absent rather than invented.
- **Consumers:** the client preserves absent creation time and the original title. The
  session picker labels the affected row; automatic CLI resume selects a healthy row or
  reports unavailable metadata separately from no matching session. Healthy rows keep their
  existing shape. List-index and selected-info storage failures
  return typed retryable 503 responses; missing selected sessions retain 404.
- **Durable truth:** unchanged. The status is a read projection and persists nothing.
