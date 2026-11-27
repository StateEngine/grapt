# Terminal and checklist workflow

Ask the agent to use `exec_command` for a command that needs a terminal or may
outlive its first output page. On Windows the default shell is `cmd`; request
an absolute PowerShell path when its syntax is needed. The command inherits
the Grapt service's parent environment under the shell environment policy:
by default all parent variables are inherited except names that look like
credentials (containing KEY, SECRET, TOKEN or PASSWORD, or configured
credential names) and launcher identity (`STATEENGINE_*`, `SECODE_STATEENGINE_*`,
`SECODE_SESSION_ID`), and configuration narrows, excludes or overrides that
through `shell_environment_policy`. Use absolute paths to executables when
the policy narrows PATH. `workdir` must stay inside the workspace.

`login` defaults to `false` after SC-SEC-11: POSIX shells use `-c`, and
PowerShell uses `-NoProfile`. Set `login: true` only when you intend startup
profiles to run; profiles may load credentials or change the environment.
`cmd` always uses `/D /S /C`, including with explicit `login: true`.

PowerShell also works with `tty: false` for commands that need no console input.
Use its ordinary absolute Windows path, such as
`C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe`. Terminal and pipe
launches resolve that path to the same approved executable. The environment is
the same filtered parent environment; with `login` false, the child's
PowerShell profile is not executed.

## Start, poll and send input

`exec_command` waits `yield_time_ms` (250–30000, default 10000) for the command
to finish. If it is still alive, the result says `status: "running"` and
returns the `session_id`: that handle is the background process, and the
command keeps running up to `timeout_ms` (default 24 h). Its later output and
stdin stay reachable through `write_stdin` with that handle. `tty` defaults to
`false`; `tty: true` attaches the command to a PTY instead of pipes, which
console programs expect.

At most eight terminals are open per session and 64 across the service. At
the limit the service first drops handles of terminals that have exited,
drained and finished cleanup, oldest first; if none qualifies, starting
another is refused with "terminal resource limit reached; finish or stop an
existing process". A terminal's slot returns only after its process exits,
its pipe drains and its cleanup completes.

This Windows example waits for one line without changing files:

```json
{
  "cmd": "set /p GRAPT_REPLY=ready^> & set GRAPT_REPLY",
  "login": false,
  "tty": true,
  "yield_time_ms": 250,
  "timeout_ms": 60000,
  "max_output_tokens": 256
}
```

On POSIX, use `printf 'ready> '; IFS= read -r word; printf '\nreply=%s\n' "$word"`
as the command. Save the returned `session_id`: it identifies this process,
and is different from the conversation's Session ID. When `status` is `running`,
use that handle with `write_stdin`; starting the command again creates another
process.

```json
{"session_id":"<returned handle>","chars":"hello\n","yield_time_ms":5000}
```

Send a line once, then poll with `chars: ""` if more output is needed. Empty
polls never rerun the command and wait 5–300 seconds; writes wait 250–30000 ms.
A single write is bounded at 64 KiB. If input delivery fails or is cancelled,
the service stops the process rather than risking a repeated write. Do not
resend input whose delivery is uncertain.

## Output pages and omitted output

Each observation reports a page of the command's total output: the page bytes
in `output`, the absolute `output_cursor` at the page's end, `has_more_output`
and `omitted_bytes`. Keep the last cursor to continue from it. An explicit
earlier `cursor` rereads a retained page; it does not repeat stdin, and a
cursor beyond the observed total is refused.

A page holds `max_output_tokens` (1–16384, default 10000) times four bytes,
clamped to 4–65536. The service retains 512 KiB of each command's output: the
first 256 KiB and the most recent 256 KiB. Output inside the evicted middle is
not retained; a page that crosses it carries the marker
`[shell: omitted N output bytes]` where the gap was, and `omitted_bytes` names
how many bytes the page skipped. Missing output is not evidence that a command
was silent.

## Standard input and interrupt

Writes go to the command's stdin as bytes; the interrupt meaning of
`chars: "\u0003"` differs by platform and by `tty`.

On Linux, `tty: false` runs the command with piped stdio in its own process
group. Exactly `"\u0003"` writes no byte: the service sends SIGINT to that
process group, so the command and its descendants receive the ordinary
interrupt. With `tty: true` the same character is written to the PTY and the
terminal line discipline raises Ctrl-C.

On Windows every terminal runs under a job object that holds the command and
its descendants from creation (`containment: "job"` in the report). With
`tty: true`, `"\u0003"` is delivered as a console Ctrl-C; if the command has
not ended within two seconds the service requests termination of the job with
Windows' Ctrl-C exit status, -1073741510, and reports the fallback: an
ancestor process may have disabled console Ctrl-C. The report states the exit
actually observed, which may differ from the requested status. With
`tty: false` there is no console, so the same character sends no signal at
all: the service requests termination of the owned job with that same status.
Treat an interrupt on a Windows pipe handle as a forced stop the command
cannot catch.

Check `done`, `pipe` and `cleanup`, as well as the exit status: process exit
alone does not prove output drained or descendants were cleaned up. On Unix,
processes that leave the command's process group can outlive it.

## Reopen and keep a plan

Compacting the conversation or reopening a client against the same live service
does not restart the command. Continue with its original handle and conversation
Session. Handles can expire after bounded eviction, and a service restart expires
them. A refusal saying the handle is absent, expired or belongs to another
Session is actionable: inspect retained history and decide whether a new command
is appropriate, rather than replaying the old one to recover output.

Use `update_plan` to replace the complete checklist:

```json
{"plan":[{"step":"Reproduce","status":"completed"},{"step":"Fix","status":"in_progress"},{"step":"Verify","status":"pending"}]}
```

Only one step may be `in_progress`. Mark steps `completed` as work finishes;
an empty plan clears the checklist. The TUI reconstructs it from the committed
tool input in Session history. Compaction and client reopen read that existing
plan; they do not execute `update_plan` again.
