# Ephemeral foreground sessions

Use `--ephemeral` for a new, foreground headless run whose Grapt session and
harness state stay in process memory:

```sh
grapt run --ephemeral --embedded --service /absolute/existing-provider-profile \
  --product /absolute/grapt-server --inspect --prompt "summarize this repository"
```

On Windows, name the corresponding absolute profile and `grapt-server.exe` paths.
The working repository is the CLI's current directory. The profile supplies
existing configuration, model metadata and credentials, read-only. Missing or
invalid inputs refuse; this mode does not set up, repair, migrate, discover
accounts or refresh tokens. Renew credentials separately before running if needed.

## What is volatile

Selection happens before installation/profile preparation, database opening,
logging, service discovery/registration and session creation. The private child
uses an in-memory event store (including SQLite temporary storage) and a shared
memory artifact store. Readiness and its random service credential travel over a
private pipe, not a registration/ready file. Harness transcripts, tool captures,
usage, permissions and plans do not persist between processes. There is no
create-on-disk-then-delete fallback. An artifact allocation exceeding its bounded
memory capacity refuses rather than spilling to disk: the shared store retains
at most 64 MiB of payload capacity and 1,024 captured/in-flight artifacts.

This is not a resumable session. Existing-session selection (`--session`,
`--continue`, `--resume`), forks, background/resident runs, worktree creation,
remote/existing-service connections and the interactive terminal are refused.
Use the explicit embedded connection above; ordinary `--embedded` without
`--ephemeral` still uses persistent session/profile state.

## Supported tools and deliberate exclusions

Ordinary reading, searching, file writing/editing, patch/apply_patch, bounded
foreground `shell`, history/plan, image viewing and web fetch remain available.
MCP, plugins/auth, configured lifecycle hooks, skills/acquisition, CodeMode,
delegation/subagents, background terminals and file undo are unavailable.
Configured executable integrations refuse instead of silently running without
retention guarantees. Volatile HTTP routes do not expose profile mutation,
provisioning, MCP sign-in or restore operations.

Automatic file checkpoint capture and the disk restore journal are bypassed
before acquisition; coding writes report `undoUnavailable`. There is no
within-process file-undo facility in this mode. Inspect or back up user files
before requesting destructive changes.

## Limits of the guarantee

Intentional repository edits, requested shell/network effects and explicit
exports or stdout/stderr redirection are user effects, **not** forbidden session
persistence. Coding replacements retain their normal atomic sibling-staging
protocol; these staged files are part of the requested user-file operation, not
harness transcripts or temporary artifact storage. Requested shell programs may
write their own caches, logs or outputs. Choose read-only permissions if that is
not wanted.

Grapt does not promise secure memory erasure, control provider-side retention,
or suppress OS swap, crash dumps, auditing, file-access metadata, terminal
scrollback or caller capture. Reading protected Windows credentials can use the
existing OS decryption helper. Provider requests still leave the process and are
subject to the provider/account policy. Do not interpret `--ephemeral` as a
provider zero-retention agreement or physical-erasure facility.

For old persistent sessions, see [session-retention cleanup](session-cleanup.md).
Cleanup is a separate offline tombstone/archive operation, not physical erasure.
