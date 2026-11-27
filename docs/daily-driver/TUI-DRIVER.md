# Driving the TUI from an agent

`grapt-tui-driver` (crate `crates/grapt-tui-driver`) runs a terminal program in a
named tmux session, sends keys, reads back the exact screen text and waits for
expected text within a bound. Use it for journey checks instead of screenshots.

Sessions are named `grapt-tui-<seat>-<journey>` on the default tmux server, so
you can watch any check live with `tmux attach -t <name>` (detach with `C-b d`).
The window keeps the size the driver gave it when someone attaches. The driver
refuses to touch sessions without that prefix.

## Commands

```
grapt-tui-driver start --seat <seat> --journey <journey> [--width N] [--height N]
                        [--cwd DIR] [--env KEY=VALUE]... -- <command> [args...]
grapt-tui-driver send --session <name> [--text <literal>] [<key>...]
grapt-tui-driver capture --session <name> [--history]
grapt-tui-driver wait --session <name> --text <expected> [--timeout-ms N]
grapt-tui-driver stop --session <name>
```

- `start` prints the session name. The default size is 120x40. The command, its
  arguments, `--cwd` and `--env` values reach the program exactly as given: they go
  into a short launcher script, and tmux only sees that script's path. `--cwd` must
  be an existing absolute directory and `--env` keys must be shell names.
- `send --text` types literal text; the remaining arguments are tmux key names
  (`Enter`, `Escape`, `Tab`, `Up`, `C-c`, `C-q`, ...) or single characters, sent
  after the text. Anything else is refused, so nothing you send can become a
  tmux command.
- Every tmux call is bounded (5 s, and never past a `wait`'s own bound) and its
  output is capped.
- `wait` prints the screen once it contains the text. It exits 1 and prints the
  last screen if the bound (default 10 s) passes or the program exits first.
- When the program exits, the pane stays so you can read its last screen and
  status. tmux moves that screen into history, so use `capture --history`.
- Exit status 2 means a usage or tmux error.

## Open a prepared session

Run `grapt` (or `grapt code`) for the packaged coding client. It launches
`grapt-tui`; `grapt-cli` is the Cargo crate name, not the executable name.
A launcher can attach R to an already running profile and an exact Session:

```sh
grapt-tui --service <profile> --session <session-id> [--directory <repository>]
```

The session's saved project, agent and model open with its transcript. Opening
preserves those choices. If a saved choice is absent or unavailable, choose an
available agent or model before composing. A supplied repository must match the
session. A missing or invalid session fails visibly; startup never creates a
replacement. Temporary read failures keep the same session for `Ctrl+o` retry.

## R composer commands

In the R frontend, `/help` lists commands and keys. `/status` opens live session,
account and usage details, and requests optional Git diagnostics for a saved
session's repository. Those diagnostics do not run during saved-session startup.
While work runs, `Tab` queues a plain follow-up and clears the composer for
another draft. `Enter` steers the current work at its next step and sends
immediately when it is idle. Pending Enter messages appear as `Steering`;
only after-turn follow-ups appear as `Queued`. The queue appears above the composer: `Alt+Up` returns the latest follow-up for editing when the
composer is empty, and `Alt+Delete` cancels it. Slash commands still use `Enter`;
`Tab` keeps its usual completion behavior when a completion is open.

Follow-ups retain their Session and attached images. They send one at a time
after successful completion. Leaving the Session, interrupting or failed work
pauses its queue for editing; an uncertain submission requires `Alt+R` to check
the original send.
The local queue holds at most 16 prompts and 16 MiB during this TUI process.

`/model` opens the F3 model and effort picker.
`/compact` schedules compaction before the next message. `/new` creates a session
with the current agent, model and effort. `/resume` opens sessions; add an exact
saved title or session ID to attach directly and restore its saved choices.

`/rename <title>` names the selected Session without interrupting its work.
From a shell, use `grapt session rename <session-id> "Title"`; it connects to
the running default-profile service, or accepts `--service <profile>` for another
profile. Titles are trimmed printable lines of at most 100 characters. New
Sessions take a title from their first nonempty prompt; a manual name is kept.

`/diff` reads the selected session's workspace changes. `/mcp` shows server
status, `/mcp verbose` shows tools and withheld names, and `/mcps` lists servers.
These results are read-only: use arrows, PageUp/PageDown or Home/End to read,
and Escape to close. Large results show an explicit truncation notice.

`/init` sends the standard repository-guidance task to the agent. It tells the
agent to preserve an existing AGENTS.md. A refused send leaves the expanded task
editable. An uncertain send retains the original input identity; Alt+R checks
or retries that original, keeping edits for a separate send after confirmation.
Commands clear only their original draft, preserving edits made while waiting.

## Cache usage

The transcript footer shows `cache read` for the latest assistant usage and
`history` for the input-weighted share across loaded assistant messages. Cache
writes count as input but not as cache reads. Separate compaction requests are
outside this history figure.

A dash means usage is unavailable, unreported or incomplete. `cause unreported`
means the provider has not explained the uncached input; new input can accompany
a cache hit, so a percentage below 100% alone does not establish a cache miss.

## Everyday TUI shortcuts

- `Enter` sends or steers; `Shift+Enter` inserts a newline. During work, `Tab`
  queues a follow-up. `Alt+Up` returns it to the draft; `Alt+Delete` cancels it.
- `Ctrl+C` interrupts a response or clears an idle draft. Press it twice within
  1.5 seconds, or send `/quit`, to exit. Quitting stops the selected Session's
  current response before closing. `Ctrl+Q` is not a quit binding in R.
- `Ctrl+P` opens saved Sessions, `F7` agents, `F3` models and effort, `F4` usage,
  and `F6` outstanding questions. Pickers support filtering, arrows and Enter;
  Escape closes them. `F2` switches the theme.
- `PageUp`/`PageDown` and the mouse wheel scroll history. `Ctrl+Home` goes to
  the beginning; `End` or `Ctrl+End` returns to the live tail. `Shift+Tab`
  focuses visible tools; Tab cycles them and Enter or Space toggles details.
- Answer a permission with `y` for Once, `n` for Reject, `s` for Session or `a`
  for Always. Use PageUp/PageDown or Home/End to inspect its preview first.
  A pending permission owns keyboard focus.
- `Ctrl+R` searches prompt history. `Alt+R` checks or retries the original
  uncertain submission, keeping later edits for a separate send.
- If startup pauses while opening the saved Session or repository, `Ctrl+O`
  retries those attachment reads. Editing, pasting or navigating cancels an
  automatic startup prompt; Enter sends the retained draft when ready.
- In a focused question (`F6`), Tab moves between fields, arrows move through
  choices and Space or Enter chooses an option. `Ctrl+S` submits the whole
  answer and `Ctrl+R` declines. Enter never submits the whole question.
- To resume a profile, run `grapt code --profile <profile> [repository]`
  without `--setup`. Before updating or reverting, quit apps and stop services
  from that installation; preserve the profile.

For workspace commands such as tests, use the Maintainer preset:
`grapt code --maintainer [repository]`. Author permits file edits and restricts
shell commands.

On Windows, the agent's default shell uses `cmd.exe`: double-quote paths
containing spaces and chain commands with `&&`. Run the PowerShell update and
revert examples below in your own terminal.

Agent shells and terminals inherit the service's environment, including developer
`PATH` and `HOME`, with credential variables removed. Start the service from your
developer terminal so `cargo` and `git` resolve normally. A running service keeps
the environment it launched with. Per-project `shell_environment_policy` can
select `all`, `core` or `none`, filter names, and set overrides; see
[shell environments](SHELL-ENVIRONMENT.md).

If a Rust command cannot find `rustc`, `cargo` or `sccache`, its result lists
installed executable candidates. Discovery checks metadata; it does not run or
install them. Choose the project's Rust version and quote its absolute path, or
correct the service's PATH. For an isolated test home, explicitly set `HOME` to a
workspace directory; it starts without downloaded dependencies or registry data.

## Updating or reverting an installed build

Quit apps and stop services from this installation first; keep the profile.
Use a verified extracted local package. Update/revert do not download packages
or migrate profiles. Check the active build with `grapt --version`.

Replace the paths below with your installation, extracted package and profile.
The explicit `--prefix` names the target installation; `--profile` makes the
older build check that profile read-only before a revert. A failed check refuses
the switch. Without `--profile`, normal repository/user profile selection applies.

Linux defaults to `$XDG_DATA_HOME/grapt`, or `~/.local/share/grapt`:

```sh
'<prefix>/current/bin/grapt' update --from '<package>' --prefix '<prefix>'
'<prefix>/current/bin/grapt' revert --prefix '<prefix>' --profile '<profile>'
```

Windows defaults to `%LOCALAPPDATA%\grapt`; custom prefixes stay inside
`LOCALAPPDATA`:

```powershell
& '<prefix>\bin\grapt.exe' update --from '<package>' --prefix '<prefix>'
& '<prefix>\bin\grapt.exe' revert --prefix '<prefix>' --profile '<profile>'
```

The Windows fixed launcher still selects the active build. If the update notice
offers a launcher refresh, quit installed apps and run its printed
`--refresh-launcher` command from the extracted package. Refresh is optional;
a newer launcher after a deliberate revert does not need a backward refresh.

## Example: the double journey

The retired `grapt-tui` executable remains available in source checkouts for
legacy fixtures; it is not the packaged R client. Its keys and rendering differ.
`grapt-tui --double` is its deterministic contract double. It needs no account
and no provider, so it is the right target for UI checks. Use real models only
when judging coding behaviour.

```
cargo build -p grapt-tui -p grapt-tui-driver
T=$(cargo metadata --format-version 1 --no-deps | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/debug
D=$T/grapt-tui-driver
S=$($D start --seat beta --journey smoke-double --width 100 --height 30 -- $T/grapt-tui --double)
$D wait --session $S --text PROJECTS
$D send --session $S Enter                # project
$D wait --session $S --text AGENTS
$D send --session $S Enter                # agent
$D wait --session $S --text MODELS
$D send --session $S Enter                # model
$D wait --session $S --text SESSIONS
$D send --session $S n                    # new session; focus moves to the composer
$D send --session $S --text "list the files" Enter
$D wait --session $S --text "session idle"
$D send --session $S C-q
$D stop --session $S
```

With the shared cache configured, binaries live in the checkout's own target
directory rather than `./target`, which is why the example asks `cargo metadata`.
