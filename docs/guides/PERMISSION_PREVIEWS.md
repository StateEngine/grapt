# Permission previews

The existing permission list and get routes can return an optional `preview`
alongside the request's ID, action, resources and state. Older requests omit it.

For shell tools, the preview describes the prepared command, its working directory
and the resolved shell program:

```json
{
  "type": "shell",
  "command": "git status --short",
  "cwd": "/work/project",
  "shell": "/bin/sh",
  "truncated": false
}
```

`classifierVerdict` is optional. An absent verdict means it was not reported.

File tools return `type: "patch"`, a `format`, a `patch` string and a `truncated`
flag. Formats are `apply-patch` for the submitted patch text, `operations` for
the proposed JSON operations, `replacement` for an edit's old and new text,
and `write` for the complete proposed file content. Replacement previews describe
the requested text change rather than claiming file line numbers.

Each preview is bounded to 8 KiB of encoded JSON, within the existing 16 KiB
permission metadata limit. `truncated: true` means the displayed command or patch
is a prefix. When the context cannot fit, `type: "unavailable"` carries a reason.
The requested resources remain available separately. A preview that would make
the complete permission metadata unreadable is refused before the ask is stored.

Preview fields describe the proposal. Approval still uses the request's ID and
the existing reply choices; preview data cannot grant permission. The underlying
prepared command and file operation remain unchanged by display truncation.

`/api/event` publishes committed permission creation as `permission.asked`,
resolution as `permission.replied`, and cancellation as `permission.cancelled`.
Cancellation reasons are `interrupted` and `approval-unavailable`; losing the last
matching approver cancels the pending request before its proposed effect runs.
Each carries the committed event identity and durable sequence. Ask and reply
references retain version 2; cancellation retains version 1. Capture replay
reconstructs the same preview and cancellation from durable facts.

The standard and R TUI approval panes wrap the complete bounded preview. Use PgUp and PgDn
to review the command or patch, working directory, shell and explanation; the
decision controls stay visible. Indentation and blank lines are preserved, and
terminal controls are escaped. Patch formats and optional classifier advice are
shown when reported; absence never implies a safe verdict. A truncated source
preview is explicitly marked. R also supports Home and End for the preview.
Each new request opens at the start of its own preview. R keeps Once, Session,
Always and Reject on fixed rows, with keys y, s, a and n respectively.
