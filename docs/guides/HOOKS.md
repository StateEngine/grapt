# Lifecycle hooks

Hooks run user-approved programs at lifecycle boundaries. V2 tool hooks can
allow, ask, deny or replace a call's input before permission checking. They are
ordinary processes under your OS account, not a sandbox. Only configure programs
you trust. Hooks cannot restart a response chain.

## Configuration

In your user `grapt.jsonc` (the service profile, global config directory, or a
trusted explicit user configuration):

```jsonc
{
  "hook_commands": {
    "policy": {
      "command": ["/usr/bin/python3", "-I", "/home/example/hooks/policy.py"],
      "timeout_ms": 10000
    },
    "audit": {
      "command": ["/usr/bin/python3", "-I", "/home/example/hooks/audit.py"]
    }
  },
  "hooks": {
    "PreToolUse": ["policy"],
    "PostToolUse": ["audit"],
    "Notification": ["audit"]
  }
}
```

On Windows use an absolute executable path, for example
`C:/Python313/python.exe`. `command` is literal argv: no implicit shell, expansion,
or quoting. To use a shell, explicitly name its absolute path and arguments.
The working directory is the **user declaration's directory**, not the project's;
relative script arguments resolve there. `cwd` in the input names the workspace.

Project configuration may bind names already declared in user `hook_commands`:

```jsonc
{ "hooks": { "PreToolUse": ["audit"] } }
```

This adds a constraint; it does not replace user bindings. Project files cannot
declare/redefine commands, change arguments/timeouts/working directories, remove
mandatory user hooks, or reorder them. Empty arrays do not disable user hooks.
Repeated bindings execute once per event. Unknown events/names or malformed
settings refuse rather than silently disabling enforcement. A profile selected
from a repository is not automatically user-owned authority.

## Tool matchers and decisions (v2)

The same configuration files also accept Claude-style command groups:

```jsonc
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "Bash|mcp__server__*",
      "hooks": [{"type": "command", "command": "python3 /home/example/hooks/policy.py", "timeout": 10}]
    }],
    "PostToolUse": [{"matcher": "Edit|Write", "hooks": ["audit"]}]
  }
}
```

A group's `hooks` may contain `type: "command"` objects or existing user-declared
command names. Inline commands are user-owned only; projects may add bindings to
user-declared names, but cannot add executable code, grant allow, or replace input
after user enforcement. Project bindings may ask or deny.
Inline strings run through `/bin/sh -c` on POSIX or Windows PowerShell with
`-NoProfile -NonInteractive`. Their `timeout` is integer seconds (1–60).
Named argv commands keep their existing executable, directory and millisecond timeout.

Matchers accept exact names, `*` and `?` globs, and `|` alternatives. Empty or
omitted matchers match every tool. They also recognize `Bash` (shell/terminal),
`Edit` (edit/patch), `Write`, `Read`, `Glob`, `Grep` and `Agent`. MCP aliases are
built from the actual published route, such as `mcp__server__search`; the tool's
real published name still appears in input and receipts. Events without a tool
ignore the matcher. Matching is case-sensitive and bounded to 1024 bytes and
32 alternatives.

V2 tool hooks receive `tool_input` in addition to the metadata below. A
`PreToolUse` hook may return:

```json
{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"git push requires a separate human action"}}
```

* `allow` skips an ordinary permission ask for this call. Explicit denies, owner
  floors, ancestor ceilings and agent tool restrictions remain in force.
* `ask` forces an attended permission prompt, including under a bypass preset.
  Without an attached approver, the call fails with an approval-unavailable reason.
* `deny` refuses before dispatch, with its reason in the model's tool result.
* `modify` requires an `updatedInput` object. Supplying `updatedInput` alongside
  `allow` or `ask` also replaces the input. The final input is bounded and decoded
  against the tool's schema before permission targets are prepared.

A top-level `decision` and `updatedInput` are also accepted. Deny beats allow;
ask beats allow across matching hooks; the last replacement is used, and later
hooks see it. Replacing input clears an earlier allow; only an allow returned
with or after that replacement can approve the new arguments. Invalid decisions and timeouts run no effect and grant no allow.
The receipt records the decision, whether input changed, and any failure reason.
Only v2 groups enable input replacement and allow/ask decisions. Existing string
bindings keep their metadata-only protocol and their allow/approve no-op behavior.

## Permission-request decisions

A user-owned v2 `PermissionRequest` group can answer an ordinary permission ask:

```json
{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}
```

Use `"behavior":"deny"` with an optional `"message"` to refuse the call and explain
why to the model. These decisions apply only to the prepared call; they neither
save permission rules nor replace tool input. `updatedInput`, `updatedPermissions`
and other decision fields are refused. Legacy string bindings remain observers.

Allow cannot lift explicit denies, owner floors, ancestor ceilings, tool
restrictions or a `PreToolUse` forced ask. Project bindings cannot grant allow.
Without a decision, the ordinary approval path continues. A timeout, invalid
output or hook failure refuses the call with a recorded reason and grants no
allow. `WaitingForInput` runs only if a permission wait still remains.

## Events and ordering

| Event | When it runs |
| --- | --- |
| `SessionStart` | When a new response chain attaches to a new or resumed session, before its first provider request. |
| `UserPromptSubmit` | Before provider planning for a promoted input batch, including an input joining an active chain. Not on transport retries or automatic continuation steps. |
| `PreToolUse` | Before final argument preparation and permission checking; an agent-excluded tool never reaches this hook. |
| `PostToolUse` | After dispatch returns, including a failed effect. Never after a `PreToolUse` refusal. |
| `Stop` | When the response chain ends (completion, failure or interrupt), before its resources are released. |
| `Notification` | After `Stop`, including when `Stop` failed, and when an assistant question waits for a user. These are lifecycle callbacks rather than OS/TUI bells or incoming mail. |
| `SessionEnd` | Once for each attached root Session when its service driver shuts down, after owned tasks settle. Resuming in another service starts a new attachment. Not on every response or child completion. |
| `PreCompact` | Before an actual compaction operation, including manual compaction. A refusal prevents that operation. |
| `PostCompact` | After compaction commits successfully. A failed compaction does not emit it; a post-hook failure cannot retry the committed compaction. |
| `SubagentStart` | When the existing execution owner starts a delegated child task, before provider preparation. |
| `SubagentStop` | After that child execution settles, including failures and interrupts. Its failure cannot change the completed child result. |
| `PermissionRequest` | When final permission evaluation would ask, before creating the approval wait. V2 groups may allow or deny this one call; remaining asks still wait for an attached approver. Assistant question waits also emit an advisory callback. |
| `WaitingForInput` | After a permission wait begins or an associated form is durably created. Unbound global forms do not invent a Session. |

Within an event, user bindings run first in source/array order, then additional
project bindings. A refusal stops the remaining commands in that event. Stop and
Notification do not grant another provider turn. At chain termination, a
Stop/Notification refusal is visible as an execution failure; it cannot undo
already completed effects.
Plugin callbacks retain their existing permission-scoped plumbing; configured
command hooks do not acquire plugin host grants.

For `request_user_input`, the durable question emits Session-scoped
`WaitingForInput`, `PermissionRequest` and `Notification` callbacks in that order.
These callbacks are advisory: an allow or deny cannot answer the question or
create a permission grant. A notifier failure is recorded for that Session and
the remaining events still run; the user's answer or decline settles the question.

## Input and privacy

Each invocation receives one UTF-8 JSON object followed by a newline on stdin;
EOF follows the object. Example:

```json
{"hook_event_name":"PreToolUse","session_id":"ses_example","cwd":"/work/project","tool_name":"edit","tool_use_id":"call_example"}
```

The last two fields appear only for tool events (including `PermissionRequest`). Input is limited to 16 KiB.
**No prompt, tool results, transcript, provider credentials or headers are sent.**
Legacy string bindings omit tool arguments. V2 tool groups additionally receive
`tool_input`, which can contain the arguments supplied by the model. Do not put secrets in hook output: output
annotations and denial reasons are visible to the model and/or transcript.

The child environment uses the user shell environment policy (inherit minus secrets),
with project policy applied only as additional removals. Project `set` entries
refuse hook startup, even for loader/interpreter settings such as `BASH_ENV`,
`LD_PRELOAD` or `PYTHONPATH`; project `inherit` and filters cannot restore values
removed by the user or another project layer. This is stricter than ordinary
shell-tool policy merging. Configured provider credential names and
KEY/SECRET/TOKEN/PASSWORD names are removed after policy application. Launcher
StateEngine identity/session prefixes and `SECODE_SESSION_ID` (case-insensitively)
are stripped after environment construction. A child cannot inherit this seat's
StateEngine identity. This filtering is not OS isolation or an exfiltration
sandbox; hook programs still have your account's filesystem/network privileges.

## Output and exit codes

* **0:** success. Empty stdout is silent; plain UTF-8 stdout is an annotation.
  JSON stdout may supply `hookSpecificOutput.additionalContext`.
* **2:** refuse. The UTF-8 stderr supplies the reason (a fixed fallback is used
  when empty). For `PreToolUse`, no effect is dispatched; the reason reaches the
  model as its tool result and is retained in the transcript.
* **Other nonzero exit, signal, timeout, invalid UTF-8/structured output, or
  output overflow:** visible fail-safe refusal, never implicit permission.

Claude-style JSON refusals are supported:

```json
{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"policy requires a different operation"}}
```

Also accepted: `{"decision":"block","reason":"..."}` and
`{"continue":false,"stopReason":"..."}`. In legacy string bindings,
`allow`/`approve` and `updatedInput` remain no-ops; v2 groups use the decisions above. A JSON-looking malformed
response fails closed, rather than becoming plain context.

Pre- and post-tool annotations join model-facing tool results; machine consumers
(including CodeMode nested envelopes, plugin interpolation/credentials and image
receipts) retain their exact output bytes. The transcript separately retains
bounded hook-result notices. A post-hook error says that dispatch already ran:
**do not retry an effect solely because its post-hook failed**.

## Bounds and failure handling

* At most 208 user command declarations and 16 distinct commands per event.
* Names: 1–128 ASCII letters/digits/`._-`; argv: 1–128 entries, at most 8192 bytes
  including terminators; executable must be absolute.
* Timeout: 1–60000 ms per command, default 10000 ms. A complete event additionally
  has a 60000 ms deadline; it cannot take 16 full command timeouts sequentially.
  `Stop`, `Notification`, `SubagentStop` and `SessionEnd` each have a stricter 5000 ms event deadline so callbacks
  finish within bounded shutdown. Root Session attachments share a five-second
  service-close allowance and are limited to 256; Session deletion discards its attachment.
* stdout and stderr: 16 KiB each, checked independently; overflow refuses, never
  parses a clipped response. Combined annotations: at most 8192 bytes per event.
* Process groups are cleaned up even when the leader succeeds, so a helper cannot
  retain an output pipe indefinitely. Timeout/cancellation drops owned I/O and
  terminates the child group. Reaping has a separate bounded two-second grace.
* Process-group fallback on POSIX cannot contain a descendant that deliberately
  escapes with `setsid`. Hooks are trusted programs, not a security sandbox.
* Results are committed as advisory session notices before the live notification.
  Session-wide waits and shutdown use an explicit Session-only notice scope,
  without inventing an execution or input identity. Hook receipts appear with
  normal tool/permission history refreshes. Notice detail is limited to 16 KiB
  with explicit truncation; a persistence
  failure refuses pre-dispatch work rather than hiding an enforcement failure.

Configuration is snapshotted when a response chain starts. Change it before the
next input/chain; do not rely on editing a running chain's policy in place.
