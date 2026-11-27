# New noninteractive runs use the caller's directory

`grapt run --new` and the default new-session run create their session at
canonicalized process cwd. An empty unscoped resume creates one there too.
The API's existing location resolver decides the project identity and applies
the project's configuration; the CLI owns neither identity nor permission.
Explicit session/project resume and `attach --new` retain their existing paths.

This closes the gap found by the J02 witness.
The omitted location dates to Rust `bb23e526`. The frozen oracle selects local
cwd in `packages/cli/src/run/run.ts:73-88`, resolves it with `localDirectory`,
and passes it to `resolveSessionTarget`. Its run command has no `--dir` flag
(`packages/cli/src/commands/commands.ts:189`). The native TUI already sends
`location.directory` in `wire/sessions.rs::encode_session_create`.

The CLI sends that location through its existing `create_session_at` method.
Unreadable, non-UTF-8 or oversized cwd is a refusal before session creation.
No new durable writer, project registry or alternate authority path is added.

The feature-enabled `public_seams` fixtures check the exact request for a new
run and empty resume, with an attach control. `scripts/release/run_directory.py`
then operates actual CLI/product binaries from a temporary repository subdir,
checks the returned directory and root-commit project ID against a distinct
service repository, and checks the inspect project cannot be widened when
`--require-narrowing` is supplied. It uses the configured loopback HTTP provider,
with isolated profile/config directories and no paid provider. Artifacts and
refusals remain in the exclusive scratch directory.

The regression mutant restores `create_session()` without a location in
`run/location.rs`. The actual subdir witness must fail naming `RUN-CWD location`
and the exact-body fixture must fail with the missing location. Restore the
source and require both green again. A mocked response alone does not prove
the project's durable identity or narrow-only configuration path.

The preset route/floor interaction is pending the Train 27 composition with
the corrected slice A. Without `--require-narrowing`, the witness leaves
that check explicitly unmeasured; a missing route cannot count as a refusal.

Linux proof: the actual corrected CLI/product run bound the caller subdir and
root project, completed the relative read, and exited cleanly. The compiled
omission mutant failed naming the service profile instead of the caller
subdir; the restored build passed the same real journey. Evidence is in
`/home/example/evidence/daily-driver/`: `run-cwd-positive-r5/witness.json`,
`run-cwd-native-mutant-r3.log`, and `run-cwd-native-restored-r5/witness.json`.

Fresh fixture profile initialization has a 90-second cap because observed
SATA fsync delays exceeded its original 30 seconds during unrelated store
tests. Actual readiness, run and drain bounds remain 15, 30 and 15 seconds.
Compiled binaries are kept outside the build cache during the real witnesses.
Earlier initialization timeouts and a missing evicted binary are retained as
unmeasured attempts; neither proves a directory regression or a restoration.
