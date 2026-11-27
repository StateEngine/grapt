# Background sessions and Git worktrees

Start a persistent service, then submit a prompt from your repository:

```sh
grapt service start --service /path/to/profile --product /path/to/grapt-server
grapt run --service /path/to/profile --background "Review the current changes"
```

The command returns the session and message IDs after admission. The service
continues execution when the CLI exits. Acceptance does not mean completion:
a queued prompt includes its blocking reason. Open the session in the native
TUI to read output and answer permission requests. The profile's service must
remain running. Background mode cannot use an embedded service or client-side
automatic permission replies (`--auto`, `--yolo`, `--dangerously-skip-permissions`).

Create an isolated checkout for a new session:

```sh
grapt run --service /path/to/profile --worktree ../review-task --background \
  "Inspect the repository and propose a fix"
```

The destination's parent directory must exist; the destination itself must not.
The checkout starts detached at the current committed HEAD. Uncommitted changes
stay in the source checkout. Running from a tracked subdirectory preserves that
relative working directory. Omit `--background` to follow the session's output
in the CLI. Worktree creation requires a local service on the same filesystem.

`--worktree` creates a new session; it cannot move a resumed or forked session.
Use the printed session ID with `--session` for later prompts. Worktrees are
retained, including after failed setup, and are never automatically deleted.

Follow output or interrupt the saved session:

```sh
grapt-tui --service /path/to/profile --session <session-id>
```

The terminal shows live progress and the completed result. Ctrl+C interrupts
active work; completed file changes remain. Use `/quit` or press Ctrl+C twice
within 1.5 seconds to close the terminal. Quitting first stops the selected
Session's current response; the separately started persistent service and
independent Sessions remain running. `grapt attach` reads Session metadata;
use the TUI for output.

After restarting the same profile's service, open the printed session ID again
to read saved results. Inspect the execution status and files before retrying
unfinished work. Continue in that session and its existing checkout with:

```sh
grapt run --service /path/to/profile --session <session-id> --background \
  "Continue the review"
```

Use `--session` to keep that checkout. `--worktree` cannot be combined with an
existing session.

## Managing checkouts in the native TUI

Select the source session or project, then use:

```text
/worktree create ../review task
/worktree create "../review task" --copy [".env.local","config/local.json"]
/worktree merge ../review task
/worktree remove ../review task
/worktree open "/absolute/backend/path/review task"
```

Paths consume the entire remaining argument, so spaces need no shell quoting.
You may instead use a JSON-quoted path (double quotes, with JSON escaping).
For `create`, the exact ` --copy ` delimiter introduces a JSON array, not a
shell list. Destinations must not exist and their parent must exist. Creation
starts detached at committed HEAD; source edits are not carried over.

Setup copies are explicit **untracked or ignored files relative to the repository
root**, never an implicit copy of local state. At most 32 files are allowed,
each at most 1 MiB and together at most 8 MiB. Backend containment and file
safety checks may refuse a copy. Inspect retained checkouts after failed setup;
the client does not delete them or retry creation.
Setup copying uses guarded descriptor-relative access on Unix (including
macOS) or pinned handles on Windows. Unsupported platforms fail closed for
nonempty setup lists; plain creation remains available. File contents are not
logged.

`merge` brings only committed changes from the named related checkout into the
currently selected source checkout, and only by fast-forward. Commit the work
first; divergence requires manual Git work. `remove` never forces removal and
refuses unmerged, current, and unrelated checkouts. Uncommitted changes and
retained untracked or ignored files also block removal; refusals distinguish
ignored files from uncommitted changes and name the affected paths. After content
checks, Git removes empty ignored or untracked directory scaffold without force.

Cleanup is supported on Linux, Windows, and macOS. It rejects descendant mount
crossings and fails closed if confinement cannot be verified. Linux requires
usable `openat2` mount confinement, with no fallback. Other operating systems
refuse cleanup because descendant mount crossings cannot be verified; inspect
the checkout and clean it up manually with Git.

Both `merge` and `remove` refuse `assume-unchanged` or `skip-worktree`
index entries, even when Git reports a clean working tree. Neither operation
changes these flags silently. Inspect the affected paths and any local edits
before explicitly clearing the flags and retrying.

`open` creates and selects a **new session** at the explicit existing directory,
using the selected agent, model, and effort. It never relocates the previous
session, and it does not send a model prompt. After `create`, use the displayed
`/worktree open` command when ready to start that session. Unlike the management
actions, `open` requires an **absolute backend path**, as printed by `create`.
The TUI never resolves paths against its own filesystem or current directory.
After creation, target metadata, catalogs, history, and questions are acquired
incrementally on the same independent lane. The source session stays selected
until the target attachment is validated; input, events, and interrupt controls
remain available while a read waits. A failed or canceled attachment reports
the known created session ID and never sends another creation request.

These effects use an independent request lane with an 80-second budget per
HTTP request in the fixed sequence; event delivery and interrupt controls remain
available. Changing session or
project discards the local result, not the backend effect. Only an explicit,
source-validated `not_started` receipt for create, merge, or remove permits a
corrected manual retry; it is shown as a refusal, not a successful operation.
A `may_have_changed` receipt preserves the backend's message and blocks another
mutation until inspection. HTTP 200 alone does not prove completion, and a
generic error—even a typed HTTP 400—does not prove that no effect happened.
Session creation may commit before its error response, so a failed `open` is
not permission to create another session.
The client never retries automatically. On an ambiguous transport/server failure,
canceled request, or unreadable success, inspect Git checkouts and the Sessions picker before
continuing. Further worktree effects are blocked for that client lifetime;
reopen the client only after inspection.
An attachment-read refusal after confirmed creation is uncertain too, even
when that read returns a typed 400; the created session is not recreated.
Staged images remain staged rather than being sent or silently discarded.
