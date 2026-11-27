# PAR-08 — Run a human's `!command`

## Status and dependency

Qwen: **yes for the TUI/client adapter, only after the permission contract lands**.
This sheet does not authorize implementing permission policy, a shell executor,
new durable events, or an API authority boundary.

Checked canonical `dev` at `5150014c16741f6b871409ce674e9cb4ff5600f8`:
`keys.rs`, `backend.rs`, and the API route vocabulary have no user-shell dispatch.
The audit's U06/PAR-08 gap therefore remains unlanded. However, the D21
candidate already implements this feature; the inspected candidate is
`b7645a4b471e78842077104a6c995264c5d3eb43`. **Do not implement a second copy.**

Before coding, fetch `main` and inspect the named interfaces below. If D21 has
landed and these acceptance cases already pass, PAR-08 needs no implementation.
If its authenticated API, permission semantics, or cancellation/receipt contract
is missing, stop at this dependency and return the missing interface to the
owner. Do not cherry-pick an unlanded candidate as an accepted dependency. If
the contract lands without the adapter, implement only the adapter described
here, reusing the corresponding D21 files and upstream notices.

## Fixed behavior

- In an empty composer, a freshly **typed** leading `!` arms shell mode. Show
  `Shell mode · Enter run · Esc literal`. Enter sends everything after the first
  `!` unchanged to the user-shell API, once, in the selected Session's recorded
  workspace. Do not send a model prompt or require an available model.
- Paste, external-editor replacement, recalled literal drafts, and restored
  literal input retain their provenance. Text starting with `!` from these
  sources remains ordinary prompt text; string-prefix detection alone cannot
  authorize execution. Removing the leading `!` leaves shell mode.
- Escape before submission exits shell mode and keeps the complete draft,
  including `!`, as a literal prompt. It sends no request. A subsequent Enter
  submits that literal prompt through the ordinary prompt path.
- Reject Enter in shell mode while images are attached, the Session is busy,
  startup policy is unresolved, or the draft's Session/directory fence differs
  from the current destination. Keep the draft and images; enqueue no effect.
- Keep the submitted draft until the same operation has a durable
  `finished/completed` receipt. A `started` row, HTTP 200 alone, a cancel 204,
  missing history, or transport failure cannot consume it. Non-completed
  outcomes keep the draft and display the failure or uncertainty.
- Escape during a pending command cancels that operation in its original
  Session, even after navigation. Keep its identity until terminal recovery;
  do not mint another identity or replay the command after reconnect.
- Committed command output becomes Session context through existing history
  reconciliation. Do not append a synthetic model message or a second receipt.

## Required permission contract — owned by D21

The accepted dependency must expose these authenticated public DTOs and routes:

```text
POST /api/session/{sessionID}/user-shell
  UserShellRequestDto { id: WireId<MessageId>, command: DtoString }
  -> 200 UserShellResponseDto { text: DtoString }
POST /api/session/{sessionID}/user-shell/cancel
  UserShellCancelDto { id: WireId<MessageId> } -> 204
```

The service, not the TUI, owns admission and validation: nonblank command,
at most 65,536 UTF-8 bytes, no NUL; duplicate IDs never execute again, including
after restart. Cancellation matches original Session, operation ID, and caller.
The command uses the ordinary `PermissionToolRunner`/shell path, selected
permission preset, descendant ceiling, sandbox, hooks, and checkpoint policy.
An explicit deny or cancelled approval launches nothing. `!` grants no bypass.

The D21 contract records system history metadata `userShell=true`,
`operationID=<id>`, and `phase=started|finished`; finished rows also carry
`outcome=completed|failed|cancelled|timed-out|dropped`. The request/approval budget
is 120 seconds, cleanup has a separate 20-second bound, and the public receipt
is bounded to 262,144 bytes. Unconfirmed cleanup keeps Session/workspace
reservations quarantined. The adapter must preserve this contract unchanged.

## Exact implementation seams

Paths are relative to the repository. These names match D21; reuse them rather
than adding the audit's hypothetical `backend/shell.rs` alongside them.

| File | Required change or reuse |
| --- | --- |
| `crates/grapt-tui/src/editor.rs` | `Editor::input` tracks fresh typed intent; `shell_mode(&self) -> bool` checks that provenance plus leading `!`; `mark_literal(&mut self)` clears shell intent. Preserve provenance in undo/redo and external-editor/paste transactions. |
| `crates/grapt-tui/src/keys.rs` | `handle_key_inner` dispatches Enter and Escape using editor provenance and existing destination/image/busy fences. `paste`/`insert_paste` must not arm shell mode. |
| `crates/grapt-tui/src/composer.rs` | `editor_rows` adds the shell-mode indication using current theme/layout helpers. No transcript layout redesign. |
| `crates/grapt-tui/src/backend.rs` | Add/reuse `Command::SubmitShell { text: String, session: Option<SessionWireId>, directory: Option<String> }` and `CancelShell`; publish `Update::shell_pending`. In the backend command loop, recheck Session, directory, startup readiness, busy state, and pending preparation before dispatch. Shell readiness must not depend on model availability. |
| `crates/grapt-tui/src/backend/user_shell.rs` | `Submission::begin<C: Connection>(&mut self, &mut Client<C>, String, bool) -> (String, Option<(String, bool, bool)>)`, `advance(...) -> Option<(String, (String, bool, bool))>`, and `cancel(...) -> (String, Option<(String, bool, bool)>)` retain draft/destination and settle the existing draft protocol. Do not own process state here. |
| `crates/grapt-tui/src/backend/first_prompt.rs` | `Submissions::submit_shell`, `cancel_shell`, and `advance` delegate to `Submission`; retain ordinary prompt/image completion handling. |
| `crates/grapt-client/src/client/user_shell.rs` | Reuse `Client::begin_user_shell(&mut self, &str) -> Result<Option<String>, ClientError>`, `poll_user_shell(...) -> Result<Option<String>, ClientError>`, `cancel_user_shell(...) -> Result<(), ClientError>`, and `user_shell_pending(&self) -> bool`. One `InFlight` owns the original typed Session ID, `FormLocation`, and `MessageWireId`, including uncertain delivery. |
| `crates/grapt-client/src/wire/user_shell.rs` | Use the public DTOs in `encode_user_shell`, `encode_user_shell_cancel`, and `decode_user_shell`; `user_shell_terminal` matches the exact operation and finished outcome in one bounded history page. No raw alternate schema. |
| `crates/grapt-tui/src/bridge.rs`, `crates/grapt-client/src/http/recovery.rs` | Retain an already-issued operator request across advisory reconnect only after endpoint, credential, and Product identity verification. Different/unverified identity never inherits the socket. Never resend the command or reset its deadline. Reuse `Bridge::begin_reconnect`, `poll_operator`, and `HttpConnection::replace_attachment`. |
| `docs/user-shell.md` | Explain typed intent, literal Escape, approval, cancellation uncertainty, bounded foreground operation, and inspection before repeating side effects. Reuse D21's guide. |

Do not put `!` parsing into `backend/commands.rs::parse`, normal API prompt
routes, startup prompts, or server-side prompt interpretation. Shell selection
must remain an explicit TUI action. Keep shared client behavior in `grapt-tui`;
the R TUI owns keys and presentation only.

## Behavioral tests and exact assertions

Reuse these D21 tests if present. Add only missing behavior at these seams;
extend the named test instead of adding duplicate variants or frame snapshots.

1. `crates/grapt-tui/src/drafts_tests.rs::explicit_shell_keys_preserve_literal_input_and_escape_without_sending`:
   paste and external-editor import of `!echo hello` both leave `shell_mode()`
   false; Enter produces `Command::SubmitImages` with `literal=true` and that
   exact text, never `SubmitShell`. Fresh key events for the same text arm shell
   mode. Escape preserves exact text, sets literal mode, and leaves the backend
   queue empty; the next Enter sends the same literal prompt.
2. In the same file, `explicit_shell_keeps_draft_and_images_when_busy_or_fenced`:
   attached image, busy Session, and changed destination each produce no backend
   command and preserve input/attachments. Restoring the original idle
   destination makes one Enter produce exactly one `SubmitShell` with the
   original text/Session/directory; a second Enter while pending produces none.
   Escape then produces `CancelShell` and retains the draft.
3. `crates/grapt-client/tests/user_shell.rs::user_shell_uses_public_dtos_retains_uncertainty_and_cancels_original_identity`:
   call `begin_user_shell("echo hello")`; assert `Operation::UserShell` and
   decoded `UserShellRequestDto.command == "echo hello"`. The adapter strips the
   first `!`; the shared client must preserve its command argument unchanged. Uncertain delivery remains pending and rejects another begin;
   navigation followed by cancel uses the original Session/ID. HTTP receipt plus
   only a started row stays unresolved; only matching finished/completed history
   clears pending and returns the receipt. A different ID or failed outcome
   cannot be treated as completed. No automatic replay occurs.

The dependency's existing
`crates/grapt-product/tests/dogfood_headless/user_shell.rs` cases must remain:
`user_shell_uses_session_cwd_and_keeps_durable_receipts_without_provider_calls`
asserts Session cwd/output/stderr/exit status, bounded large output, malformed
input/deny refusal, no denied file creation, duplicate refusal across restart,
one side effect, and zero provider calls;
`user_shell_cancel_is_exact_and_settles_pending_permission` asserts wrong
Session/ID refusal, exact cancel 204, no stale permission approval or provider
call. The cleanup/quarantine regression remains owned by D21. Do not weaken
these tests to finish the adapter. Preserve the existing real-TCP reconnect
test `bridge::user_shell_reconnect_tests::advisory_reconnect_keeps_the_issued_user_shell_only_for_the_same_product`.

## Refusals and scope limits

On invalid input, denied authority, unavailable attachment, busy/changed
destination, unconfirmed submission, or uncertain cleanup: keep the original
draft, surface the owning error, and execute no fallback command/model prompt.
Do not auto-retry cancellation as execution. Do not interpret receipt absence
as proof that the process never ran.

Do not change Product permission/sandbox/checkpoint code, API DTOs/routes,
durable write paths, database schema, command-ID semantics, global config,
credentials, provider calls, or process cleanup policy in this Qwen adapter.
Do not add dependencies, a second shell runner, PTY/background support, timers
for polling, or new persistent state. Changes to the required contract go back
to its owner for Tier 1 review.

## Verification

For implementation after the dependency lands (not for this docs-only sheet):

```sh
cargo fmt -p grapt-tui -p grapt-tui --check
cargo clippy --locked -p grapt-tui -p grapt-tui --all-targets -- -D warnings
cargo test --locked -p grapt-tui --test user_shell --no-fail-fast
cargo test --locked -p grapt-tui explicit_shell_ --no-fail-fast
cargo test --locked -p grapt-tui advisory_reconnect_keeps_the_issued_user_shell_only_for_the_same_product --no-fail-fast
cargo test --locked -p grapt-types --test no_god_files --test direct_include_format --no-fail-fast
cargo test --locked -p grapt-store --test write_chokepoint --no-fail-fast
```

Run the normal affected gate before landing. Operate the installed TUI: type
`!git status --short`, confirm one receipt in the selected workspace, exercise
Escape before submission and during a pending approval, and verify a pasted
bang remains literal. Product contract tests belong to dependency qualification;
this adapter does not require duplicating its full security run.

## Reuse source

Read and retain the existing D21 adaptations and notice fragments
`notices/10-adoptions/user-shell-direct.md` and `user-shell-frontend.md`.
The approved upstream is `openai/codex` at
`092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`:
[provider-free user-shell task](https://github.com/openai/codex/blob/092d3acd6bec3e3a14bdc7e7a2810ab628ab759d/codex-rs/core/src/tasks/user_shell.rs),
[composer](https://github.com/openai/codex/blob/092d3acd6bec3e3a14bdc7e7a2810ab628ab759d/codex-rs/tui/src/bottom_pane/chat_composer.rs),
and [Apache-2.0 license](https://github.com/openai/codex/blob/092d3acd6bec3e3a14bdc7e7a2810ab628ab759d/LICENSE).
Reuse explicit input selection and provider-free dispatch; retain Grapt's
ordinary permission/sandbox contract rather than upstream's full-access policy.
