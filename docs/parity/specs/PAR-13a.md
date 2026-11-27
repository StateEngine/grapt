# PAR-13a — Read sessions, models and recorded usage from scripts

Implementation sheet for Qwen; no product change is included in this document.
Baseline: `dev` at `5150014c16` on 2026-11-05. At that commit,
`CliCommand::parse` handles session rename/provisioning but has no session-list
or model-list arm. The audit's K06/PAR-13a gap is still open.

## Deliverable and boundaries

Implement one small CLI slice: `grapt session list` and `grapt models`, in
text and JSON, using the running service's existing authenticated HTTP client.
Session JSON includes the API's existing recorded usage fields. Do not invent a
separate usage accumulator. Aggregate stats, transcript export and a new
`session usage` command are separate follow-ups, not requirements of this slice.

No HTTP route, database, permission, model discovery, provider request or durable
write changes. No new dependencies. No server auto-start, profile creation,
stdin read, approver attachment, provider login or token refresh initiated by
the CLI adapter. The server retains its existing catalog-read behavior.

## Existing owners to reuse

| Owner | Read/change |
| --- | --- |
| `crates/grapt-cli/src/command.rs` | Add `pub mod catalog`, `CliCommand::Catalog(catalog::CatalogCommand)` and `CommandError::Catalog(&'static str)`; dispatch session-list before the existing session provision fallback and dispatch models directly. Extend the missing-command diagnostic without changing existing commands. |
| `crates/grapt-cli/src/command/catalog.rs` (new) | Closed grammar, action/options types and `parse_sessions(arguments: &[String])` / `parse_models(arguments: &[String])`, both returning `Result<CliCommand, CommandError>`. |
| `crates/grapt-cli/src/command/help.rs` | Add `Topic::SessionList` and `Topic::Models`, exact standalone help recognition, help text and main usage entries at the existing usage owner. |
| `crates/grapt-cli/src/client.rs` | Declare private `mod catalog`; leave `sessions()` and `top_level_sessions()` unchanged for their existing callers. Reuse `send_json`, bounded decoding, credential handling and transport. |
| `crates/grapt-cli/src/client/catalog.rs` (new) | Add typed `ApiClient::session_page(&SessionListOptions) -> Result<SessionsResponseDto, ApiClientError>` and `ApiClient::models_at(&str) -> Result<ModelListResponseDto, ApiClientError>`; both `pub(crate) async`. |
| `crates/grapt-cli/src/executor.rs` | Declare private `mod catalog` and delegate the new command arm to `catalog::execute(command, limits).await`. |
| `crates/grapt-cli/src/executor/catalog.rs` (new) | Connection, one read, thin output projection and emission; `pub(super) async fn execute(command: CatalogCommand, limits: CliLimits) -> u8`. |
| `crates/grapt-cli/tests/catalog_commands.rs` (new) | Parser, transport and binary-output witnesses; use `#![cfg(feature = "loopback-testkit")]`, the existing `tests/support/mod.rs` scripted HTTP fixture, and a local copy of the disposable discovery setup pattern from `tests/public_seams/compatibility/rename.rs`. |
| `crates/grapt-cli/tests/contracts.rs` | Extend the existing poisoned-profile help witness to include the two new standalone help forms. |

Use these exact command types in `command/catalog.rs`, with the same derives
as other command types (`Debug, Clone, PartialEq, Eq`):

```rust
pub struct CatalogCommand {
    pub connection: ConnectionChoice,
    pub action: CatalogAction,
    pub output: OutputMode,
}
pub enum CatalogAction {
    Sessions(SessionListOptions),
    Models { directory: String },
}
pub struct SessionListOptions {
    pub limit: std::num::NonZeroU16,
    pub cursor: Option<String>,
    pub project: Option<grapt_types::ProjectId>,
    pub directory: Option<String>,
    pub search: Option<String>,
    pub include_children: bool,
}
```

No general catalog framework or alternate HTTP client. The command module owns
the admitted options; the client module translates them into the existing API.
Keep output structs and renderers private to `executor/catalog.rs`.
For a portable cancellation witness, put the workflow behind private
`async fn execute_with_cancel<F>(command: CatalogCommand, limits: CliLimits,
cancel: F) -> u8 where F: std::future::Future<Output = ()>`. `execute` supplies
`async { let _signal = super::termination_signal().await; }`; a unit test supplies
a notification future. Use `tokio::select!` over that future and the ordinary
workflow, with cancellation returning `ExitClass::Cancelled.code()`. This seam
does not change signal ownership or introduce a second command implementation.

## Exact grammar

```text
grapt session list CONNECTION [--limit N] [--cursor TOKEN] [--project ID]
                    [--directory SERVER_DIRECTORY] [--search TEXT]
                    [--include-children] [--json]
grapt models CONNECTION --directory SERVER_DIRECTORY [--json]

CONNECTION := --service ROOT
            | --remote HTTPS_ROOT --credential FILE
```

Flags may appear in any order after the command/subcommand. Every flag is
single-use. Require exactly one connection mode; the remote mode requires both
its flags. Reuse `ConnectionChoice::{Discover, Remote}` and existing endpoint
and credential validation. `--embedded` and `--product` are refused. An omitted
connection is a usage error; this slice does not select a default profile.

`--limit` defaults to 50 and accepts only ASCII decimal digits representing
1..=128. Reject signs, whitespace, zero, fractions and overflow. Project IDs use
the existing `ProjectId` parser, with no extra prefix or ASCII assumption:
these IDs are opaque strings. Directory and cursor values must contain
1..=4096 UTF-8 bytes, with no control characters. Search accepts 1..=256 UTF-8
bytes with no control characters. Directory is an opaque server-side path:
do not canonicalize it on the CLI host or inspect a remote filesystem locally.
The existing server location validation remains authoritative.

Refuse positional words, duplicate flags, missing values, unknown options,
`--jsonl`, `--format`, prompt/run/session-selection/permission flags, and
session-only flags on `models`. Use existing `Duplicate`, `MissingValue`,
`UnknownOption`, `InvalidProject` and `ConnectionMode` errors where applicable;
use `Catalog` for the remaining named grammar/bound errors. All parse failures
exit 2 before service discovery, credential reads or stdin reads.

`grapt session list --help` and `grapt models --help` exit 0 with focused help
and no I/O except stdout. Recognize only those standalone forms, also with
`-h`; do not reinterpret a flag value as help. Keep session rename, provision,
run and ACP parsing unchanged. Do not introduce a `sessions` alias.

## HTTP contract and bounds

For local commands call existing `executor::connect` with `Discover`; it uses
`ServiceDiscovery::discover`, not `ensure` or `connection_client::open`.
Remote commands use the existing `Remote` branch and HTTPS checks. Once
connected, execute exactly one catalog GET; no catalog retry loop or SSE feed.
Reuse termination handling to cancel discovery/read/emission and exit 130.

Session listing requests `GET /api/session` with:

| Option | Query |
| --- | --- |
| Always | `order=desc`, `limit=<admitted decimal>` |
| Default | `parentID=null` |
| `--include-children` | Omit `parentID` entirely; do not send an empty/null query value. |
| Other provided options | `cursor`, `project`, `directory`, `search` respectively |

Encode all query values with `url::form_urlencoded::Serializer` or the existing
URL query-pair owner. Never concatenate a supplied value into a URL. Decode
`grapt_api::SessionsResponseDto`; reject a page containing more rows than the
requested limit as a protocol error. Do not sort, deduplicate or silently
truncate returned rows. Never follow `cursor.next` automatically. One invocation
returns one page, at most 128 rows; the caller supplies the returned opaque
cursor to the next invocation. This also bounds request count and memory.

Models request `GET /api/model?location[directory]=<encoded directory>` and
decode `grapt_api::u3a::ModelListResponseDto`. This API is a single bounded
catalog, not a paginated collection: retain its existing `MAX_MODEL_ROWS`
(2048) bound and do not add a fictitious cursor or use session pagination flags.
Preserve API row order, disabled rows and every variant ID. The TUI decoder at
`crates/grapt-client/src/wire/catalogs.rs::decode_models` expands those variants
for its picker; the CLI exposes them nested under their base model.

Reuse `ApiClient`'s existing response-size/DTO checks and
`BoundedOutput`/`CliLimits` for all output. Accumulate and validate the single
result before emitting success output. Do not read the service DB, persisted
transcript files, provider credentials or configuration catalogs directly.

## Exact output and failure behavior

JSON sessions: one JSON object, no prose or progress, with
`schema: "grapt.cli.session-list.v1"`, `data` equal to the decoded API page's
data, and `cursor` equal to its cursor object. Preserve omitted versus null
cursor fields through the DTO serializer. An empty successful page has
`data: []` and the actual cursor, exits 0, and is distinct from a failed read.

Preserve each Session's existing `tokens`, `cost`, `costStatus` and `metrics`
without recomputing them or filling omissions. In particular, absent tokens
after an `exceeds-bound` metrics read stay absent; missing/unreadable cost is
not zero; `known-nonbillable` zero stays distinguishable from an API-price
estimate/subtotal. This is the initial script-readable usage surface. Do not
calculate a cache percentage, aggregate lifetime totals or estimate dollars.

JSON models: one object with `schema: "grapt.cli.model-list.v1"`, the API
`location`, and `data` containing only these fields per base model:
`id`, `modelID`, `providerID`, `name`, `enabled`, `status`,
`limit: {context, output}`, and `variants` as an array of variant ID strings.
All come from typed getters. Omit `settings`, `headers`, `body`, `package`,
credentials and all other model fields even if the API supplies them.
This allowlist is intentional: never serialize the full model DTO to stdout.

Text sessions start with exactly `ID\tPROJECT\tUPDATED\tTITLE\n`, followed by one
row per returned Session: ID, project ID, API updated timestamp and title.
Text models start with exactly
`PROVIDER\tMODEL\tENABLED\tVARIANTS\tNAME\n`; each row uses providerID, id,
`true`/`false`, variant IDs and name. Separate columns with tabs. Serialize
every opaque string column as a JSON string literal: session ID, project ID,
title, provider ID, model ID and model name. Serialize the variants cell as a
JSON array of ID strings, including `[]` for no variants; never comma-join
opaque IDs. The updated timestamp remains a finite numeric cell and enabled
remains `true`/`false`. This escaping applies even to typed IDs: their opaque
string validation does not prohibit tabs, newlines or terminal escapes.
Preserve API row order. Empty text
results contain the header only. For a present non-null next session cursor,
also emit `Next cursor: <JSON string literal>\n` on stderr in text mode only.
JSON mode emits no successful stderr text. Use existing bounded sink methods.

Before success emission, failures leave stdout empty and put a bounded
diagnostic on stderr. Never present stale rows or an empty-success envelope
for HTTP errors, invalid DTOs or a stopped/unreadable service. Reuse these
existing exits: usage 2; discovery/health attribution failure 10; malformed or
oversized successful HTTP response 11; request transport or non-success HTTP
status 12; output failure 74; cancellation 130. HTTP 401/403 must fail, with no
anonymous retry, credential value, Authorization header or error-body echo.
Keep the existing remote-error scrubber/diagnostic conventions; do not print a
raw `reqwest::Error` containing the supplied query when a bounded generic
catalog diagnostic suffices. Broken pipes use the existing output-failure path.

## Required tests and exact assertions

Use disposable profiles/scripted loopback HTTP only. No live profile, paid
model, credential changes or native-PC lease is needed for these witnesses.
Keep every existing CLI and permission assertion. New tests belong in
`tests/catalog_commands.rs` unless a private renderer needs an adjacent unit
test; extend the existing help witness in `tests/contracts.rs`.

1. `catalog_grammar_is_closed_and_bounded`: assert both valid command variants,
   default limit 50, explicit limit 128, typed project, cursor/directory/search
   preservation and JSON mode; reject limits `0`, `129`, `-1`, `+1`, `1.5`,
   ` 1`, an overflowing integer, duplicate flags, mixed/incomplete connections,
   invalid project ID, over-bound directory/cursor/search values,
   control-containing directory/cursor/search values and every refused flag
   class above. Assert parse failures have no fixture-server requests.
2. `catalog_help_never_opens_a_profile_or_reads_stdin`: add both standalone help
   forms to the existing poisoned-profile/empty-PATH/open-stdin helper. Assert
   exit 0, correct usage text, empty stderr and unchanged scratch inventory.
3. `session_list_reads_one_exact_page_and_preserves_usage`: discovery fixture
   plus one GET; assert only `GET /api/health` and `GET /api/session` occurred.
   Decode the captured query and assert order desc, limit 2, parentID null and
   the exact supplied valid project ID and search/cursor/directory values.
   Include `&`, `%`, `+`, spaces and a non-ASCII character in the latter values
   as data rather than extra keys; do not put them into a typed project ID.
   Return two distinct rows and a next cursor; assert schema, row order,
   equality of data/cursor with the fixture, and zero automatic next-page GETs.
   Include one real token/cache/cost subtotal and one row with metrics
   `exceeds-bound`, absent tokens/cost and `costStatus: "unavailable"`; assert
   omissions survive. Add known-nonbillable zero in a second page invocation
   and assert its status and cost remain distinct. With `--include-children`,
   assert the request has no parentID and a returned child row is preserved.
4. `models_use_server_location_and_emit_only_safe_fields`: start with a valid
   full `ModelListResponseDto` fixture (copy the JSON shape from
   `crates/grapt-client/src/wire/catalogs.rs::model_value`, not the ACP test
   fixture's minimal untyped model JSON; add no TUI dependency). Include enabled
   and disabled rows, variants, and a distinct secret sentinel in
   settings/headers/body.
   Assert only health plus the one model GET, the exact decoded directory
   query, API ordering/identity/enabled/status/limits/variants, and absence of
   every excluded field and sentinel from stdout and stderr. Assert an empty
   valid catalog exits 0 with `data: []`, not a no-model refusal.
5. `catalog_empty_and_error_are_distinct`: valid empty session/model responses
   succeed; HTTP 401, 403, 500, disconnected response, malformed JSON,
   wrong-DTO JSON, excessive row count and oversized response all exit with
   the class specified above, empty pre-emission stdout and nonempty bounded
   stderr. A stopped, missing or health-mismatched service exits 10 and never
   launches a child or changes the disposable profile. Credential sentinels
   never appear in either output stream.
6. `catalog_text_is_stable_and_safe`: assert the exact headers/column order.
   Include valid DTO project/provider/model/variant IDs containing tabs,
   newlines, commas and ESC, as well as a title/name with those characters.
   Assert every opaque string cell is the corresponding JSON string literal,
   the variants cell is a JSON array preserving the exact original ID strings,
   and decoding each cell recovers its input without splitting or loss. Assert
   one physical LF-terminated record per row, only the header-defined number
   of raw tab separators, and no raw ESC or CR bytes. Also assert the text-only
   JSON-quoted next-cursor stderr notice and header-only empty results.
7. `catalog_output_and_waits_remain_bounded`: exercise rendering with a small
   `CliLimits` sink; exceeding stdout bytes returns the existing output error
   before the oversized record is written. A held scripted HTTP read cancelled
   through `execute_with_cancel` exits 130 without a success envelope; put this
   cancellation witness in the executor module's unit tests, inject a notify
   only after the scripted peer has received the GET, and assert the result is
   `ExitClass::Cancelled.code()`. The production wrapper supplies the real
   termination future. In both cases, no provider, prompt, form, mutation or
   event-stream route is contacted.

## Verification and delivery

For implementation, use the admitted heavy launcher and the affected gate on
the actual implementation base; do not run Cargo from this docs-only branch:

```sh
source scripts/build-cache.sh
heavy --nvme --owner par13a-verify -- python3 scripts/build/gate.py affected \
  --base 5150014c16 --timeout 5400 --log /tmp/par13a-affected.log
```

After a rebase, substitute its accepted base for `5150014c16`. The affected
gate must cover CLI fmt, clippy `--all-targets -- -D warnings`, ordinary and
loopback-testkit tests, direct dependents and the three law checks. While the
native hold remains active, do not launch a gate that starts a native sidecar.
For a focused development run under the same heavy scheduler, the exact new
target is `cargo test --locked -p grapt-cli --features loopback-testkit --test
catalog_commands --no-fail-fast`; the existing contracts test remains required.
One independent reviewer ACCEPT and the actual affected gate are required to
land implementation. No implementation gate is requested for this document.

Check current main before starting: if equivalent commands have landed, update
the ticket rather than creating another adapter. This sheet does not authorize
a new route, dependency, permission change, aggregate stats/export or bypass of
a later native-compute hold.

## Reuse basis

Read OpenAI `openai/codex` at
`bb64b894fd67158be0f874872e6d7bfe717e7368`, Apache-2.0 (`LICENSE` checked):
`codex-rs/tui/src/app_server_session/thread_list.rs` sends a typed list request
and propagates refusal; `codex-rs/app-server/src/models.rs::supported_models`
projects the existing catalog instead of rediscovering models in a CLI.
Reuse those ownership choices through Grapt's existing typed HTTP DTOs;
do not copy Codex's legacy CWD retry loop or its provider model fields.
No upstream code is copied by this sheet. If implementation copies/adapts code,
retain Apache attribution, record the repo/path/commit in its commit body and
add the required notice fragment; do not edit generated notices.
