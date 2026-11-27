# Permissions: presets

A Session runs at a **permission preset**: how much the agent may do without
asking you. Maintainer is the interactive default; bypass is an explicit choice
for unattended work.

| Preset | Reads and searches | Creating and editing files | Commands (shell) | Overwriting, moving or deleting a file | Anything else |
| - | - | - | - | - | - |
| `inspect` | allowed | denied | denied | denied | denied |
| `author` | allowed | allowed | denied | asks | asks |
| `maintainer` (default) | allowed | allowed | destructive commands ask | allowed inside the project | allowed |
| `bypass` (explicit only) | allowed | allowed | allowed | allowed | allowed |

Creating, editing, moving or deleting a permission authority source asks under
`author` and `maintainer`. This includes `service.conf`, `grapt.json(c)` and the resolved
`GRAPT_CONFIG` path, whatever its filename. Every path in a patch is checked;
one denied path refuses the whole patch before any effects. `inspect` denies
mutations. Remembered approvals never lift a deny or the authority-source ask.

## Repository profiles

An existing repository `.grapt` directory is a candidate, not a trusted user
profile. `grapt code` asks before using it. Without a terminal, it refuses
unless the exact canonical path and configuration were previously trusted, or
you explicitly select that profile with `--profile`.

After reviewing a profile, `grapt code --profile <profile> --trust-profile`
remembers the decision under `grapt/profile-trust` in your user configuration
directory, outside the opened repository. Changing `grapt.json`, `grapt.jsonc`,
`service.conf` or including adding or removing one of those
files, invalidates it. `--profile <profile> --revoke-profile-trust` revokes the
decision and exits without starting a service. Selection without `--trust-profile`
applies only to that launch.

Selection and trust do not promote repository configuration to user-global
authority. Repository permissions can only narrow the user's policy; their
MCP/plugin process declarations remain withheld rather than silently acquiring
spawn consent. File tools cannot read, search or modify `.grapt` contents,
including nested profile directories and searches with `include_ignored`.
These file-tool protections are not an operating-system shell sandbox.

## Windows profile protection

New profiles and account-staging directories receive an owner-only, inheritable
DACL before configuration or credentials are written. Persisted trust decisions
are accepted only from protected storage; a foreign or unverifiable owner/ACL
refuses rather than silently trusting existing records.

Provider keys and ChatGPT access/refresh tokens use Windows DPAPI **CurrentUser**
protection in the existing profile files. The key remains with Windows, not in
the repository or another Grapt credential store. Atomic replacement writes
only protected bytes to the private temporary file. Legacy ChatGPT state is
migrated under the existing renewal lock before contacting a provider; explicitly
imported legacy API keys are protected in the new profile. A damaged protected
file never falls back to plaintext. Moving a protected profile to another
Windows account or machine requires signing in again or explicitly supplying
the account's credential. Protection failure refuses the write and preserves
the previous generation. An already private directory is verified without
rewriting its ACL, including repeated starts by a non-elevated user.

Legacy ChatGPT upgrades prepare both protected files before publishing either.
Their exact originals remain in the private `.chatgpt-protection-upgrade` slot
until the pair commits. A failed publication restores both originals; after a
process interruption, restart the new build so it completes recovery before
downgrading. Leave that recovery directory intact. Unix sign-in persistence is
unchanged.

If an earlier failed upgrade left only some files sealed, the new build can read
that mixed state under the owning Windows account. To restore an older build's
plaintext format instead, stop the service and back up the profile, then run
Windows PowerShell from a source checkout:

```powershell
powershell -NoProfile -NonInteractive -File scripts/windows/recover-legacy-sign-in.ps1 -Profile E:/path/to/profile
# After the check reports recoverable, explicitly restore the legacy format:
powershell -NoProfile -NonInteractive -File scripts/windows/recover-legacy-sign-in.ps1 -Profile E:/path/to/profile -RestoreLegacy
```

The script uses DPAPI locally, makes no provider request and retains encrypted
originals in private `recovery-sealed` files. It imports Windows PowerShell's
bundled modules explicitly, so a `PSModulePath` inherited from PowerShell
7 cannot redirect `Get-Acl` module loading.

## The default is `maintainer`

Maintainer allows ordinary file work, MCP tools and external tools. Its short
exception list asks before destructive shell commands (including recursive
directory deletion and writes outside the project), `external_directory` access,
and edits to permission authority files. Native file tools keep their project
path confinement. With no approver attached, an Ask refuses immediately.

## Choosing a preset

- Choose with `grapt run --inspect`, `--author`, `--maintainer` or
  `--dangerously-bypass-permissions` (also available to `grapt code` and
  `grapt-tui`). The choice
  is saved on that Session and applies to later runs too. Several flags resolve
  to the strictest.
- As your default: `permission_preset=<name>` in the profile's `service.conf`,
  or `"permission_preset": "<name>"` in your global user config. The
  profile's `service.conf` wins over the global config.
- A repository's own configuration may only make the preset **stricter**: a
  project's `"permission_preset"` that is stricter than your default applies; a
  looser one is ignored. An explicit user-owned `bypass` selection skips
  repository permission restrictions as well; a repository cannot activate it.

Delegated Sessions inherit the parent's effective ceiling. A child selection
cannot widen it, and child configuration may narrow it further at every depth.

## Explicit bypass

`--dangerously-bypass-permissions`, `permission_preset=bypass` in `service.conf`,
or `"permission_preset": "bypass"` in user configuration disables every permission
Ask and Deny, including authority-file protection and configured rules. It is
never the default. The TUI footer shows `BYPASS: permission checks disabled`.
Authentication and native tool path confinement still apply. Use it only when you
intend the agent to run unattended with this authority; combine trust flags only
when you intend the strictest flag to win.

## `--auto`

`grapt run --auto` (or `--yolo`) answers each pending request "once", except
one that would delete or move a file, including within a patch. Both automatic
modes refuse changes to permission settings and requests whose classification
is unavailable. `--dangerously-skip-permissions` also approves ordinary removal
requests. Permission settings require an explicit interactive decision.

An Ask needs an attached approver. The TUI attaches while its event stream is
connected; automatic CLI modes attach for their selected Session before
submitting work. An ordinary headless run does not attach an approver. If nobody
can answer, the tool immediately returns `ApprovalUnavailable` with
`approval unavailable (headless)` and executes no effect for that call. The
same result reaches nested CodeMode calls and remains in durable tool history.
Configured Allow and Deny decisions retain their behavior.

External approvers opt in on their authenticated `/api/event` subscription
with `X-Grapt-Approver: <session ID>`, or `*` when they handle every Session
visible to that principal and location. An ordinary event subscription is only
an observer. The attachment ends on disconnect or stream overflow. Losing the
last matching approver cancels unresolved Asks; reconnecting cannot revive
them. Already-settled decisions remain effective.

## Configured rules

Use `permission` in the same global, profile or project JSON/JSONC documents
that choose the preset. An effect (`allow`, `ask` or `deny`) applies to every
resource of an action; an object applies effects to resource patterns in order.
Within one document, the last matching rule wins. For example:

```json
{
  "permission": {
    "shell": { "*": "allow", "*production-secret*": "deny" },
    "edit": "ask",
    "webfetch": "deny"
  }
}
```

Resource patterns use `*` for any run and `?` for one character; file-tool
resources are resolved paths. `edit` covers creating, modifying, overwriting,
moving and deleting files. `bash` names the `shell` action; `web_fetch` and
`view_image` name `webfetch` and `read`. A rule may also name an MCP tool.
Rules in `agent.<selected agent ID>.permission` follow that document's general
rules and apply only to the Session's selected agent.

User-owned rules may lift an ordinary preset ask. Project rules only narrow;
project asks and ancestor limits remain floors outside bypass. Neither configuration nor
remembered approvals lift a deny or an authority-file ask. Configured denials
apply to both the permission API and real tool effects, including each path of
a patch. A malformed permission document refuses resolution rather than
silently restoring defaults.

Each resolution allows up to 1,024 configured rules, with patterns of at most
4,096 bytes. A delegated lineage allows at most 4,096 configured rules across
its existing 64-parent bound.

For a launcher-bound MCP server named `stateengine`, place this rule beside
its user-owned server declaration in the seat profile's `grapt.jsonc`:

```json
{
  "permission": {
    "stateengine_*": {
      "stateengine/*": "allow",
      "stateengine/*/*": "ask"
    }
  }
}
```

The action matches the published tool name; the resource matches the original
server/tool pair. Keep the second entry: ordinary patterns cross separators,
so it prevents a different server such as `stateengine/other` from receiving
the grant. Tools containing separators continue to ask. The profile is loaded
before the launcher's `GRAPT_CONFIG` document; an MCP-only launcher document
preserves the profile's rules. Later explicit permission rules may override
them, and configured narrowing still applies.

## Literal command prefixes

Use `execpolicy` alongside `permission` to match command arguments after shell
quote removal. The program name is exact: `git` does not match `/tmp/git` or
`Git`. Later positions accept a literal or an array of literal alternatives;
`*` and `?` are literal characters here. Trailing arguments are permitted.

```json
{
  "execpolicy": [
    { "pattern": ["git", ["status", "diff"]], "decision": "allow" },
    { "pattern": ["git", "push"], "decision": "ask" },
    { "pattern": ["rm"], "decision": "deny" }
  ]
}
```

`agent.<selected agent ID>.execpolicy` adds rules for that agent. The strictest
matching prefix decision wins. An allow must cover every simple command,
including commands inside transparent wrappers. Environment assignments,
unmodeled wrapper options, supplied shell or PowerShell startup state,
variables, globs, redirected writes, unread scripts, terminal input and
expressions exceeding the reader's bounds cannot receive a prefix grant.
File locks, tracing options, options targeting other processes and input-fed
commands (`xargs`, `parallel`, `fd -x`, `find -exec`) use the ordinary policy
decision. A known ask or deny still applies when another part is incomplete.
Ordinary permission rules continue to apply when no prefix decision covers
the expression.

For `exec_command`, a command-only prefix grant also requires the default
shell's native invocation with profiles disabled. A different shell executable
or a profile-loading invocation keeps the ordinary decision; known prefix
asks and denials still apply.

Within each document, prefix decisions follow ordinary rules. They share that
document's authority: user rules can lift asks, project rules only narrow, and
neither can override a deny or an ancestor's ceiling. Prefixes and ordinary rules
share the 1,024-rule resolution and 4,096-rule lineage limits. A prefix permits
at most 64 argument positions, 64 alternatives per position and 4,096 bytes.
