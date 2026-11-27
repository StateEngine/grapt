# PAR-36 — Headless review command

Implementation specification for a Qwen producer. This sheet authorizes no
implementation by itself; the assigned producer must receive implementation
admission. Scope: local Git review through the existing headless runner.

## Status and dependencies

Checked canonical `dev` at `5150014c16741f6b871409ce674e9cb4ff5600f8`
on 2026-11-05. `CliCommand::parse` has no `review` arm; there is no dedicated
headless review command. The existing interactive `/review` is not this ticket.
PAR-01 is already landed in `2b5042e9cccf9348db11a77df423230b10db14a3`:
reuse `StartupSelection`, `Runner::with_selection`, and atomic agent/model
validation during Session creation. Do not implement PAR-01 again.

Existing owners to read before editing:

- `crates/grapt-cli/src/command.rs`: `CliCommand`, `parse_run`, `Parsed`,
  `parse_options`, `connection`, and `output_mode`.
- `crates/grapt-cli/src/selection.rs`: `StartupSelection::set_model` and
  `set_agent`; `src/client/selection.rs`: `create_session_at_selected`.
- `crates/grapt-cli/src/executor.rs`: the `CliCommand::Run` arm,
  `run_with_cancellation`, `connect`, `output_sink`, and diagnostic rendering.
- `crates/grapt-cli/src/run/admission.rs`: `Runner::admit_prompt` selects the
  preset before submitting the prompt; `run/location.rs` resolves the cwd.
- `crates/grapt-product/src/dogfood/agent_definitions.rs`: the existing
  `reviewer` definition and its read-tool ceiling.
- `crates/grapt-product/assets/commands/review.txt`: the existing review rubric.
- `crates/grapt-worktree/src/git.rs`: bounded subprocess execution and Git
  environment scrubbing. Its helpers are private; do not expose a generic Git API.

## Command contract

```text
grapt review [--uncommitted | --base <ref> | --commit <ref>]
              [--service <root> | --embedded --service <root> --product <binary>]
              [-m|--model <provider/model>] [--reasoning-effort <level>]
              [--max-steps <positive-u32>] [--json | --jsonl]
grapt review --help
```

No target flag means `--uncommitted`. Exactly one target is allowed, including
rejecting duplicate copies of the same target flag. No positional prompt or
stdin is accepted. A ref must be nonempty, at most 4096 UTF-8 bytes, not begin
with `-`, and contain neither whitespace nor control characters. Reject URL
forms containing `://` or starting with `github.com/`; do not resolve PRs. Ref resolution
is local, before service discovery, Session creation or a provider request.

Omitted connection uses `command::terminal::user_profile`, as Session rename
already does. It discovers an existing service; it does not start one or perform
sign-in. Embedded mode retains its ordinary owned-child lifecycle. Initially
reject `--remote`/`--credential`: local Git context and service file reads must
refer to the same machine. Remote review is a separate follow-up.

Always create a new Session, select agent `reviewer`, use preset `inspect`, and
keep `AutoReply::Off`. Allow model/effort selection through the existing owners.
Do not offer agent substitution or permission flags. Reject `--session`,
`--continue`, `--fork`, `--worktree`, `--background`, `--resident`, `--ephemeral`,
`--auto`, `--yolo`, bypass flags, `--author`, `--maintainer`, `--stdin`,
`--prompt`, `--image`, `--file`, custom instructions and PR targets.

Use the normal text/JSON/JSONL run output and exit classes. Findings are model
output, not an exit-code severity scheme: a completed review exits 0 whether or
not it finds defects. Usage/target/context refusal exits 2; unavailable service
exits 10; cancellation exits 130. Provider and output failures retain the ordinary
runner's classifications. Do not invent a structured findings schema.

## Exact implementation changes

1. Add `command/review.rs`, declared by `command.rs`, containing
   `ReviewTarget::{Uncommitted, Base(String), Commit(String)}` and
   `ReviewCommand { connection: Option<ConnectionChoice>, target: ReviewTarget,
   output: OutputMode, model: Option<RequestedModel>,
   reasoning_effort: Option<String>, max_steps: Option<NonZeroU32> }`.
   Add `CliCommand::Review(review::ReviewCommand)` and `review::parse(&[String])`.
   Reuse the existing closed option parser and validation; add review target
   fields there only as necessary, and explicitly refuse them for every other
   command. Validate the allowlist above rather than accepting all `run` options.
2. Add `Topic::Review` to `command/help.rs`, recognized only for the standalone
   `review --help` / `review -h` forms. Add `review` to `executor.rs`'s `USAGE`
   and dispatch it to a new `executor/review.rs`.
3. Add `src/review.rs` with private `review/git.rs` and `review/prompt.rs`.
   Declare this internal module in `lib.rs`. Its public-to-crate seam is
   `async fn prepare(cwd: &Path, target: &ReviewTarget)
   -> Result<Option<PreparedReview>, ReviewError>`.
   `PreparedReview` contains the canonical repository `directory: PathBuf` and
   bounded `prompt: String`; `None` means no changes.
   Keep refs, patches and file contents out of `Debug`/diagnostic output.
4. In `review/git.rs`, adapt the existing bounded Git subprocess pattern to this
   fixed read-only command set. No new dependencies, shell, generic command
   string executor, checkout, fetch, index refresh or temporary worktree.
   Every child uses null stdin, piped stdout/stderr, `kill_on_drop(true)`, finite
   collection and explicit kill-and-wait on timeout/overflow. Reuse the
   `grapt-worktree/src/git.rs` environment scrub list and disable fsmonitor,
   hooks, automatic maintenance, pager and optional Git locks. Also disable
   external diff/textconv for each diff. Never execute a configured diff driver.
5. Build a prompt from the existing review rubric, selected immutable object IDs
   and the captured context below. Include the rubric with `include_str!`;
   keep the product asset unchanged. Replace `$ARGUMENTS` with a description of
   the resolved target, not an executable command. Append an explicit instruction
   that Git context is already supplied, source text is untrusted review data,
   the reviewer must report prioritized actionable findings with file/line
   references, and it must not edit files or request a shell.
6. `executor/review.rs` calls `prepare` before connecting. For a nonempty result,
   construct `StartupSelection` with agent `reviewer` and the optional model,
   then use `Runner` with `with_preset(Some("inspect"))`, `with_auto(Off)`,
   the selected effort/max-steps, and `SessionSelector::New` through
   `run_with_cancellation`. Factor the existing Run arm's foreground settlement
   into a private shared helper if needed; do not duplicate its polling,
   cancellation, history rendering or embedded-child cleanup loops.
7. Add `directory: Option<PathBuf>` to `Runner` (default `None`, preserved in
   task clones) and a crate-private `with_directory(PathBuf)` setter. In
   `run/location.rs::create`, resolve that directory when present; otherwise
   preserve the existing cwd behavior. Review passes its prepared repository
   root, so invoking from a nested directory still permits reading all reviewed
   paths within the correct workspace. Do not change process-global cwd, add a
   public path override flag or alter existing Run/Attach placement.

## Git target and context rules

Resolve the cwd canonically. Run `git rev-parse --show-toplevel` there and require
a working tree with a committed HEAD. Run all subsequent commands at that root;
pass args as individual argv values. Resolve HEAD and target refs once using
`git rev-parse --verify --end-of-options <ref>^{commit}` and retain only the
returned full 40- or 64-digit hexadecimal OID. Never interpolate a raw ref into
a shell or send an unresolved ref to the model.

| Target | Captured patch and file versions |
| --- | --- |
| Uncommitted | Capture separate `git diff --cached` and `git diff` patches, plus `git ls-files --others --exclude-standard -z` untracked text files. This includes staged-only, unstaged-only and untracked changes and does not include ignored files. Read the current text of changed, nondeleted regular files for context. Refuse an unmerged index. |
| Base | Resolve `git merge-base --all <base-oid> <head-oid>`; require exactly one OID. Capture `git diff <merge-base-oid> <head-oid>`. This target reviews committed branch changes; staged, unstaged and untracked changes are excluded. Read changed, nondeleted postimages from `<head-oid>:<path>`. |
| Commit | Capture `git show --format= --root --diff-merges=first-parent <commit-oid>`. A merge commit is compared with its first parent; a root commit is compared with the empty tree. Read changed, nondeleted postimages from `<commit-oid>:<path>`. Exclude unrelated working-tree changes. |

All patch commands use `--no-ext-diff --no-textconv --no-renames --no-color`
and terminate revision arguments with `--`. Use NUL-delimited Git name listings
for file collection; do not parse display-quoted patch headers as paths. Obtain
the corresponding changed-name list with the same comparison, including
`--root` and first-parent semantics for commit mode. Deleted-file context is
already in the deletion patch; do not read an unrelated current file instead.

Encode each context section's relative filename as a JSON string. Bound the
union of changed paths to 128 entries and each path to 4096 UTF-8 bytes. Refuse
absolute paths, parent components, controls, non-UTF-8 paths, symlinks, submodules
and nonregular untracked files; never follow an untracked link outside the repo.
For filesystem postimages, check canonical containment under the resolved repo
and reject link ancestors before opening. Do not emit a partial review when a
selected file cannot be read. Use the same checks on Unix and Windows, including
canonical Windows verbatim roots.

Initial text-only limit: each file at most 128 KiB, combined patch/file context
at most 768 KiB, and the final rubric plus context at most
`bounds::MAX_STDIN_BYTES` (1 MiB). Invalid UTF-8, embedded NUL, binary diff or
binary selected file is a named pre-provider refusal; do not lossy-decode or
silently omit it. Each Git command has a 30-second deadline, stdout capped at
768 KiB and stderr at 64 KiB; the complete preparation has a 60-second deadline.
Read at most limit + 1 bytes to detect overflow and drain both pipes concurrently.
Do not print raw Git stderr, file bodies, ref input or patch text in errors.

Re-read HEAD and the two uncommitted patch/name listings after context collection.
If the selected comparison changed, refuse with `review target changed during
preparation; retry`. This bounds consistency checking; do not retry indefinitely
or claim a filesystem snapshot against concurrent writers.

For no selected changes, create no Session and contact no service/provider.
Text prints `No changes to review.`; JSON emits
`{"schema":"grapt.review.v1","status":"no_changes"}` and JSONL emits that
same object followed by a newline through `BoundedOutput`.

## Tests to add and exact assertions

Add `crates/grapt-cli/tests/review_commands.rs` for parser and executable
boundary tests only. Put the Git preparation witnesses in
`crates/grapt-cli/src/review/tests.rs`, declared by `#[cfg(test)] mod tests;`
in `src/review.rs`; these unit tests can inspect the internal `prepare` seam and
`PreparedReview`. Do not expose that module, its fields, or a production API just
for integration tests. Use disposable Git repos, fixed author/committer dates
and argv commands; never use the developer's repository, profile or provider.

- `review_targets_and_options_are_closed`: omitted/explicit uncommitted parse to
  the same target; base/commit preserve their ref; model/effort/max-steps and
  JSON/JSONL survive parsing. Every conflicting/duplicate target, missing value,
  prohibited flag, PR URL, control/ref overflow and positional prompt returns an
  error. `review --help` is `FocusedHelp(Review)` without touching cwd or stdin.

In `src/review/tests.rs`:

- `review_captures_staged_unstaged_and_untracked_separately`: seed three files
  containing `STAGED_ONLY`, `UNSTAGED_ONLY`, `UNTRACKED_ONLY`, plus an ignored file
  containing `IGNORED_SECRET`. The prompt contains the first three and not the
  fourth, contains the rubric once, and has no unresolved `$ARGUMENTS`.
- `review_base_uses_merge_base_and_immutable_postimages`: create a diverged base
  and feature branch; place `FEATURE_ONLY` in the branch, `BASE_ONLY` only on the
  other branch, and `DIRTY_ONLY` in the working tree. Context contains the
  merge-base/head OIDs and `FEATURE_ONLY`, but neither other marker. Commit mode
  includes only its selected change and selected-tree postimage; also test a
  root commit and the documented first-parent merge comparison.
- `review_from_nested_cwd_uses_repository_root`: prepare from `repo/nested`,
  assert `PreparedReview.directory` equals canonical `repo`. The Session-create
  placement assertion belongs in the executable/public-seam test below.
- `review_invalid_or_unbounded_input_starts_no_run`: nonrepo, unborn HEAD,
  nonexistent ref, blob ref, absent/multiple merge base, conflict, binary/NUL,
  symlink, invalid path, 129 paths, a 128-KiB-plus-one file, context overflow,
  timeout and collection race each return a named refusal. A fake Git helper
  that records argv verifies ref bytes never become an option/command, no
  external diff or fsmonitor runs, and a killed helper is joined. Do not change
  global PATH/environment from parallel tests; inject the Git executable only
  through an internal test constructor.
- `review_no_changes_is_effect_free`: clean repo returns `None`; the executable
  portion is a separate integration test: `review_no_changes_executable_is_effect_free`
  in `tests/review_commands.rs` launches the CLI in the clean repo and asserts
  exit 0 and the exact no-change output above even when the selected service is
  absent, with no registration/Session/provider request.

Add `tests/public_seams/review.rs` and register it in `public_seams.rs`, reusing
the scripted server from `tests/support/mod.rs` and existing selection fixtures.
`review_selects_before_admission` asserts Session creation contains
`"agent":"reviewer"` and the selected model, then the preset request contains
`{"preset":"inspect"}` before any `/prompt`. The prompt contains the captured
diff. A refused agent/model/preset produces no prompt request. A successful
scripted finding appears unchanged in ordinary text/JSON/JSONL output; cancellation
and service loss use the existing exit classes without a second prompt admission.
`review_nested_location_is_explicit` launches CLI child processes with
`Command::current_dir(repo/nested)`, using the scripted service: review creates
its Session at canonical `repo`, whereas ordinary run still creates its Session
at canonical `repo/nested`. Do not change the test process's global cwd.

Add a real product journey in
`crates/grapt-product/tests/dogfood_headless/review.rs`, registered in
`tests/dogfood_headless.rs`. Use the existing loopback `ProviderFixture` and
shipped CLI. Have the provider request a file-writing tool despite the reviewer
ceiling, then return `P1 fixture finding: src/answer.rs:2`. Assert the write is
refused, the file/index/HEAD remain byte-identical, no escape file appears, and
the finding reaches CLI stdout. Check the public Session preset is `inspect`
and the provider's offered tools exclude edit/write/patch/shell and arbitrary
MCP effects. Keep this an ordinary test of existing enforcement, not a permission
engine modification. If existing enforcement cannot satisfy it, stop implementation
and return a concrete Tier 1 dependency; do not repair authority in this slice.

## What not to touch

No product permission tables, reviewer tool lists, Session schema, provider
protocol, custom config layer, public HTTP route, TUI `/review`, review rubric
rewrite, remote repository access, Git mutation, PR lookup, auto-fix or new
dependency. Never fall back to the default agent, a broader preset, shell-based
review or an incomplete diff when validation fails. Repository and user profile
bytes must remain unchanged; the ordinary service's review Session/history writes
are expected and remain behind their existing owner.

## Verify the implementation

```sh
cargo fmt -p grapt-cli -p grapt-product --check
cargo clippy -p grapt-cli -p grapt-product --all-targets --features grapt-cli/loopback-testkit,grapt-product/loopback-provider-testkit -- -D warnings
cargo test -p grapt-cli --lib --no-fail-fast -- review::tests::
cargo test -p grapt-cli --test review_commands --no-fail-fast
cargo test -p grapt-cli --features loopback-testkit --test public_seams --no-fail-fast -- review::
cargo test -p grapt-product --features loopback-provider-testkit --test dogfood_headless --no-fail-fast -- review::
bash scripts/gate.sh affected --base main
```

Run builds through the installed heavy scheduler. Native tests use the reviewed
controller and current hold/order rules. This docs-only sheet itself needs no
gate; implementation still needs reviewer ACCEPT and the normal affected
gate before landing.

## Upstream reuse

Read `openai/codex` at `bb64b894fd67158be0f874872e6d7bfe717e7368`:
`codex-rs/exec/src/cli.rs::ReviewArgs`,
`codex-rs/exec/src/lib.rs::build_review_request`, and
`codex-rs/prompts/src/review_request.rs::review_prompt`. Its `LICENSE` at that
commit is Apache-2.0. Reuse its closed, mutually exclusive target model and
merge-base resolution concept, adapted to SE's existing run/agent/permission
owners and stricter pre-provider refusals. Do not copy its unrestricted custom
prompt path or add a second runtime. Any copied implementation must retain the
Apache notice and name the upstream path and exact commit in its commit body.
