# Start coding

Extract all package files together, then open a terminal in your repository.
Run the packaged CLI using its full path. To install both commands for your user:

```sh
/path/to/package/grapt update --from /path/to/package
```

On Windows, run this in PowerShell:

```powershell
& 'C:\path\to\package\grapt.exe' update --from 'C:\path\to\package'
```

Upgrade across the rename with the new package's own binary: run the extracted
package's `grapt` (or `grapt.exe`) to install, as above. An installed pre-rename
`secode` refuses a grapt package before it stages anything, and that refusal is <!-- grapt-rename: keep -->
expected. By default the new build installs into a fresh `grapt` prefix beside
the old one and leaves the pre-rename installation untouched as a fallback;
installation prints one line naming it. Point launchers and seat configs at the
new prefix when ready. Existing profiles are found where they are: a
`~/.config/secode/default` profile is used while no `grapt` one exists. Within a <!-- grapt-rename: keep -->
prefix, pre-rename versions stay verifiable, so `grapt revert` and
`grapt update --prune` keep working across the boundary.

Installation publishes `grapt` and, when bundled, `se`, without administrator
access or a separate Node installation. Open a new terminal afterward. Linux
launchers live in `~/.local/bin`; if it is absent from PATH, installation prints
the exact `export PATH=...` line to add to your shell configuration. Windows adds
the fixed launcher directory to your USER Path, preserving its registry type.
If that directory already appears behind other USER Path entries, move the
existing entry to the front and retry; the installer will not move an entry it
did not add. Choose an installation path without `%` when USER Path supports
environment-variable expansion, so Windows cannot resolve it to another directory.
Windows searches System Path first. Installation refuses a conflicting `se` or
`grapt` there; resolve that conflict and retry. It never changes System Path.

`se --version` reports the bundled StateEngine version. StateEngine is a separate
public project (currently AGPL-3.0); each bundled version is governed by the
LICENSE and third-party notices shipped with that version, in `stateengine/LICENSE`
and `stateengine/THIRD_PARTY_NOTICES.md`. Grapt's MIT OR Apache-2.0 license
does not cover it. The release manifest says whether the component is included;
older or unbundled builds report that it is unavailable.

`grapt revert` switches both commands to the previous installed package.
`grapt update --prune` retires old inactive packages. `grapt uninstall` removes
only this installation's owned command aliases or USER Path entry, retaining
versions and all profiles for recovery. Pre-existing and user-replaced aliases or
PATH entries are retained. Linux keeps the shared `~/.local/bin` PATH entry.

For a portable Windows installation, add `--no-path` to `update --from`.
This creates the verified installation and command launchers without reading or
writing USER Path. Add the installation's `bin` directory to PATH in the current
terminal only. Pass `--no-path` again on update, revert and uninstall to keep
USER Path untouched; uninstall retains versions and profiles. This option does
not remove a PATH entry added by an earlier ordinary installation.

After installation, copy the printed PowerShell start command to run immediately
without changing PATH. For example, with an installation in
`C:\Users\example\AppData\Local\O'Brien Tools\Grapt`:

```powershell
& 'C:\Users\example\AppData\Local\O''Brien Tools\Grapt\bin\grapt.exe' code
```

Alternatively, prepend that installation's `bin` directory for this PowerShell
session only, preserving the existing PATH:

```powershell
$env:PATH = 'C:\Users\example\AppData\Local\O''Brien Tools\Grapt\bin' + ';' + $env:PATH
grapt code
```

Use your own installation path (the default is `$env:LOCALAPPDATA\grapt`).
Keep the `&` and single quotes around an executable path; double any apostrophe
inside a single-quoted PowerShell path, as above. The session-only recipe does not
write USER or System Path and is not saved when you close the terminal.

Update an installed portable package with its own command:

```powershell
grapt update --from 'C:\path\to\next-package' --no-path
```

Internal packages predating `--no-path` were never public releases and are not
the supported portable-upgrade baseline.

Windows sign-in recovery is included in `grapt`; no source checkout is needed.
After quitting apps, stopping the profile's service and backing up the profile,
check recovery without changing credentials, then explicitly restore the legacy
format only when returning to an older package:

```powershell
grapt recover-sign-in --profile 'C:\path\to\profile'
grapt recover-sign-in --profile 'C:\path\to\profile' --restore-legacy
grapt revert --profile 'C:\path\to\profile' --no-path
```

Recovery runs as the signed-in Windows account, requires a private profile and
retains encrypted originals beside restored files. It makes no provider call.

To run without installing, use the extracted CLI directly:

Linux:

```sh
/path/to/package/grapt
```

Windows PowerShell:

```powershell
& 'C:\path\to\package\grapt.exe'
```

If the package directory is already on your PATH, run:

```text
grapt
```

`grapt code` opens the same terminal.

Use the same executable path in the commands below if it is not on your PATH.

`--profile` chooses the profile. Otherwise, an existing repository `.grapt`
is reused; a fresh repository uses `grapt/default` in your user configuration
directory. The terminal starts the product automatically for the current directory
and stops its owned product when you quit. Choose an agent, then a model and
effort to start a new session. Ctrl-P opens saved sessions. Type a task and press Enter. Permission requests require your answer.
The HTTP API also exposes [bounded approval previews](../guides/PERMISSION_PREVIEWS.md)
and permission lifecycle events.

Exit 13 reports "the provider rejected this key"; check or replace the key
at its provider before trying again. The key is never included in the message.
Other remote connection failures exit 12; invalid command input exits 2.

### Provider slots

`provider_active_attempts` in the profile's `service.conf` bounds simultaneous
root requests (1–64, default 1). Children use an independent shared pool, so a
child cannot consume its parent's provider slot. They reuse the same provider
credentials and transport clients.

User configuration can set `"provider_capacity": {"children": 3}` (1–64).
The default is three children: four total executions minus
the root. Restart the service after changing the user budget. Project config
may lower that budget, and cannot raise it. Requests sharing the child pool
keep FIFO order; a lower project budget may wait for other children to finish.
These are local concurrency limits, not measured provider allowances.

A rate limit temporarily reduces child concurrency and pauses both root and
child admission on that account for its bounded backoff. Other accounts remain
available. Successful completions restore child capacity gradually, up to
the user's limit. R shows **Provider busy, retrying.** during automatic retry;
the first rate limit does not by itself fail a child.

A queued session shows **waiting for provider slot (position k)**.
Requests run in FIFO order across sessions. Waiting remains cancellable and
continues while the adapter makes progress. If it stalls past the inactivity
bound, the parent receives a visible retryable failure; `followup_task` can resume the child with
its existing work. Stopping a foreground parent's task also cancels its queued
children.

### Event streams

The service admits sixteen HTTP event streams by default, separately from its
32 ordinary request slots. Both live events and capture streams use that budget.
Set `max_inflight_streams` in the profile's `service.conf` and restart the service
to change it. The maximum is 32 with the service's 64-connection limit; zero
disables HTTP event subscriptions. A full stream budget returns HTTP 503 with
`Retry-After: 1`, while ordinary calls keep their own capacity.
The service logs a warning for each new stream admitted above 75% occupancy,
including the occupied and configured slot counts.

Healthy streams stay open and receive keepalive comments after 15 idle seconds.
A blocked write has a 30-second allowance; disconnects and write failures release
the stream's capacity. Linux also bounds unacknowledged data with a TCP user
timeout. Half-open detection on Windows uses TCP keepalive and is best-effort.

### Permissions and presets

Every Session runs at a permission preset: how much the agent may do without
asking you. Maintainer is the default. It allows read-only inspection and
workspace file creates/edits (`fs.create`, `fs.modify`), and asks before:

- shell commands (whether ordinary or destructive);
- file overwrites, moves, and deletes (`fs.overwrite`, `fs.move`, `fs.delete`);
- the `external_directory` action, access to a directory outside the project;
- MCP, plugin, or unknown tools;
- creating, editing, moving or deleting the permission authority files: the
  profile's `service.conf`, `grapt.json` or `grapt.jsonc` (and the
  pre-rename `secode.json` or `secode.jsonc`), and the resolved <!-- grapt-rename: keep -->
  configuration documents.

The other presets, one line each: `inspect` allows read, search and query and
denies every mutation. `author` allows creating and editing files, asks
before overwriting, moving or deleting one, runs no commands and denies the
external directory.

Bypass disables every permission check, including authority-file protection
and configured rules. It is never the default. It is activated only by: the
`--dangerously-bypass-permissions` flag (accepted by `grapt code`,
`grapt run` and `grapt-tui`); `permission_preset=bypass` in the profile's
`service.conf`; or `permission_preset=bypass` in a user-classified
configuration document: the in-use profile's own `grapt.json` or
`grapt.jsonc` (which live in the repository when the repository's
`.grapt` is the default profile), your user configuration, or the
`GRAPT_CONFIG` and `GRAPT_CONFIG_CONTENT` launch sources (the pre-rename
`SECODE_CONFIG` and `SECODE_CONFIG_CONTENT` are accepted, with a notice, when <!-- grapt-rename: keep -->
the `GRAPT_` name is unset). Documents in
the repository's working tree are classified Project; they can narrow a
preset but not widen it. While it applies, the TUI footer shows `BYPASS:
permission checks disabled`. Use it only for explicitly unattended work.

An ordinary headless run attaches no approver; an ask there is refused at
once, and the tool returns `approval unavailable (headless): no matching
approver is attached; no effect ran`. Configured allow and deny decisions
keep their behavior. `exec_command` runs in a non-login shell by default;
`login: true` loads startup profiles.

[Permissions: presets](../daily-driver/PERMISSIONS.md) covers configured
rules, remembered approvals and execpolicy.

On first start with a fresh profile, Grapt opens its **Connect a provider** dialog.
Choose a provider and enter an API key, or choose OpenAI and sign in with ChatGPT
using a browser or device code. Anthropic uses API keys. No environment variables
or separate login command are required. Keys are hidden while typing and the engine
checks and stores credentials in the profile.

Press Ctrl+O to open the sign-in page, or Ctrl+Y to copy its link. Complete sign-in
in your browser and return to Grapt; Esc cancels. Ctrl+A or `/connect` reopens
connections at any time to add, rename or disconnect accounts. F3 opens the model
picker after connecting; choose an agent and model to start chatting.

For a local OpenAI-compatible provider, set `provider.protocol_family` to
`openai-compatible` and `provider.endpoint` to its API base URL. The separate
`provider.family` identifies the model family, such as `qwen3`. Without
`catalog.source`, Grapt reads the account's `account-models.json` snapshot;
a listed model's positive `max_model_len` supplies its context limit. An optional
`model-catalog.json` can supply other metadata. Missing or invalid inventory
produces a startup error naming the input to fix.

To use an existing profile in a different directory:

```text
grapt code --profile <profile-directory> <repository-directory>
```

For a fresh profile, import an existing supported account configuration:

```text
grapt code --setup <account-directory> --profile <new-profile-directory>
```

The account directory contains `grapt.jsonc` (or, for an account from before
the rename, `secode.jsonc`), `provider.credential` and any <!-- grapt-rename: keep -->
model inventories or additional account inputs named by that configuration.
Setup copies those explicitly named inputs into a new profile. It preserves the
existing account directory and makes no provider call. The chosen account is used
when you submit work. Setup refuses an existing destination. If setup fails after
creating a profile, preserve it and choose another unused destination.

Local MCP servers named `stateengine` load schemas only when needed by default.
Set `"codemode": false` on that declaration to offer its tools directly. Other
local MCP servers opt in with `"codemode": true` under `mcp`. Tools are discovered through
`execute`: `return catalog.search("query")` finds names and short descriptions,
and `return catalog.describe("tool_name")` returns the argument schema. Calls use
`await tools["tool_name"](arguments)` with the same permission checks. Discovery
results join the conversation when requested; the initial tool description stays
fixed when that server's catalog changes.

Project instruction edits keep the initial prompt prefix and append chronological policy
updates in conversation history. Native Claude uses System updates on supported
models; unknown models and retained legacy signed-thinking history keep the
existing current-prefix format until that history leaves the active context.

Use F7 for agents, F3 for models and effort, Ctrl-P for saved sessions and F4
for usage details. Esc interrupts active work; Ctrl+C interrupts a response or
clears an idle draft. Press Ctrl+C twice within 1.5 seconds, or use `/quit`, to
close the terminal after stopping the selected Session's current response.
Run `grapt` again and select a session to resume its durable history.
An unavailable model requires an explicit replacement; the terminal preserves
the saved choice.
Use Left/Right, Home/End and Delete to edit, Ctrl-Left/Right to move by word,
and Ctrl-W or Alt-D to kill a word. Ctrl-U/Ctrl-K kill to the line boundary;
Ctrl-Y yanks the last kill. Up/Down recall complete prompts from the selected
session when the draft is empty. Ctrl-R searches that history: Enter keeps the
match for editing, Escape restores the draft, and a second Enter sends it.
Alt-R retries an unconfirmed original prompt. `/help` lists these bindings.

Type these commands in the composer and press Enter:

| Command | Action |
| --- | --- |
| `/status` | Show the selected model, effort, account and reported context/cache usage. Missing usage stays unavailable. |
| `/model` | Open the model and effort picker. |
| `/new` | Start a session with the current agent, model and effort. |
| `/resume` | Open the session picker; `/resume <title or id>` restores a saved session and its choices. |
| `/rename <title>` | Rename the selected session. |
| `/undo` | Review the last file effect; `/undo apply` applies the reviewed undo. |
| `/rewind [n]` | Reopen an earlier prompt for editing without changing files. |
| `/quit` | Close the terminal. |
| `/compact` | Request compaction before the next input. |
| `/diff` | Inspect staged, unstaged and untracked changes in the selected backend workspace. |
| `/mcp` | Show MCP server status and tool counts; `/mcp verbose` lists tools and withheld names. |
| `/init` | Submit a repository-guidance task through the ordinary prompt path, preserving an existing `AGENTS.md`. |

`/diff` and `/mcp` inspect current state without sending a model request. Oversized
or unreadable diffs report an error. MCP tool counts describe server publication;
session permissions and policy also apply. Resource inspection and sign-in are
separate surfaces. `/init` requires a session and uses the usual tool permissions.
Assistant replies render Markdown headings, lists, emphasis, code blocks and
tables. Narrow tables show each record as labelled values. User messages and
tool output stay literal; reopening a session renders the original saved reply.
Answers that exceed formatting limits show their source with a visible notice.

`grapt code --help` lists these options. The package contains the R terminal
frontend; the retired terminal executable is not included.

### Permissions and presets

A repository's own `.grapt/` configuration remains project-scoped and cannot
grant itself user-global trust or enable permission bypass.

On Windows, `grapt update` and `grapt revert` first check that only you,
SYSTEM and Administrators can change the installation (its directory, `bin`,
`versions` and selection records). If another account can write there, the
update is refused, nothing changes, and the message names the access and an
`icacls` command that removes it; Grapt never changes permissions itself.
"Could not verify install permissions" means the check itself could not run,
for example under a PowerShell language mode other than FullLanguage; that
refuses too. The check runs at update, revert and first install, not every
time Grapt starts.

### Dependency licenses

Install `cargo-deny` 0.20.2 and `cargo-about` 0.9.2 from their official releases
(or `cargo install --locked --version VERSION TOOL`). The affected and nightly
gates run license, dependency-ban and source checks; missing tools fail the gate.

After changing dependencies, run `python3 scripts/release/licenses.py generate`
under the normal heavy queue and commit the updated
`licenses/dependencies/NOTICE.html`. Generation covers every platform and feature,
including build dependencies, and retains full license and copyright texts.
`about.toml` prefers Apache-2.0 when offered; `deny.toml` restricts CDLA permission
to the approved CA certificate data. Missing published MIT texts have exact-version
entries in `scripts/release/license-overrides.toml` with their pinned upstream source.
A new unresolved copyright notice fails generation and needs its actual published text.

The packager copies the register and other `licenses/*/*` files into every target
archive, and refuses a missing register or changed dependency manifests, lockfile,
license policy or copyright overrides until the register is regenerated.
The bundled Node runtime license remains a separate required packaging input.
