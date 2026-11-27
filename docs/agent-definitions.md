# Named agents

Grapt includes primary agents `build` and `plan`, and the legacy general
agent `default`. Plan inspects the repository and writes Markdown plans only
under `plans/`; it cannot run arbitrary shell commands. These restrictions use
the permission system. Explicit permission bypass skips them, including the
plans-only write restriction; use Plan without bypass when you need that boundary
enforced. The literal `plans/` path and its target
components must not be symlinks or reparse points.
Grapt also includes `worker`, `explorer` and `reviewer`. Explorer and
reviewer use a read-only tool list. All children inherit their ancestors'
permission ceilings.

Children may delegate again when their tools and permissions allow it. There is
no separate shallow delegation limit: authorization follows at most 64 parent
sessions, and execution refuses a deeper lineage. This supports multi-stage
workflows while bounding permission lookup and retaining every ancestor's
permission ceiling.

Child provider concurrency defaults to 3. Set `provider_capacity.children` in
your user or service `grapt.jsonc` to an integer from 1 to 64. A project setting
can only lower that limit. Additional children wait for provider capacity;
this limits simultaneous child provider calls, not the total number of child
sessions. Provider or account limits may reduce concurrency further.

Add a TOML file in your user config's `agents/` directory (normally
`~/.config/grapt/agents/`) or a project's `.grapt/agents/` directory:

```toml
name = "api-reviewer"
description = "Review API contracts and failure handling."
developer_instructions = "Review the assigned API changes. Report concrete defects with file references."
model = "provider/model" # optional; must be a connected model
tools = ["read", "glob", "grep", "read_history"] # optional, exact tool names
mode = "subagent" # primary, subagent or all; custom agents default to all
steps = 30 # optional positive step cap; an exhausted cap stops execution
disabled = false
```

Call `spawn_agent` with `agent_type = "api-reviewer"` to select it. Omitting
`agent_type` inherits the parent's agent. Agent listing and session agent
selection expose these same definitions. A primary-only parent must name an
enabled subagent explicitly when delegating. A session's explicit model wins over
its agent's model. For a child, an explicit Subagent model choice wins, followed
by the child's agent model, then the parent's model.

Definitions are ordered by name. User definitions override named built-in fields;
project definitions override user fields, with the nearest `.grapt` directory
winning. Each format merges supplied fields with the previous definition;
omitted fields inherit. A supplied prompt or model replaces that field as a
whole. Project files can only narrow the tool lists of lower-priority definitions; omitting
`tools` keeps a lower list in force. Duplicate names in one directory are errors. The existing composed runtime identities
cannot be replaced.

`tools` is an optional exact-name allowlist; an empty list disables all tools.
It restricts both the advertised tools and execution, including nested and
plugin-host calls. Children also obey every ancestor's list, even when they
select another agent. Listing a tool never grants permission to use it. A tool-restricted agent's
shell commands and terminal input require owner approval: arbitrary host code
can rewrite its own definition. Prefer the read and search tools for inspection.
Explicit permission bypass still applies. For
MCP CodeMode, allow `execute` and each nested tool's published name.

Native Markdown definitions also load from `agent/` and `agents/`, including
subdirectories. The filename without `.md` supplies the default agent name; an
explicit `name` field can override it. YAML frontmatter accepts `name`,
`description`, `model`, `tools`, `color`, `mode`, `steps`, `disabled` and `hidden`.
Supply instructions in the body, or in `system` or its `developer_instructions`
alias, using one source per file. TOML accepts the same fields; its files remain
direct children of either directory. README.md documentation is skipped.
User-owned agent directory links are followed, and their resolved directories remain protected authority paths. Linked project roots and nested project directories are refused with their path. Definition-file links are refused; unrelated file links are ignored.

Configuration documents use `agents` to define or override roles with the same
fields. The legacy singular `agent` key accepts only per-role `permission` and
`execpolicy`; definition fields there are refused with guidance to use `agents`.
Explicit null fields are refused; omit a field to inherit its previous value.

```json
{"agents":{"build":{"steps":30},"api-reviewer":{"description":"Review API contracts","system":"Review API compatibility and report concrete defects.","mode":"subagent","tools":["read","grep"]}}}
```

Definitions follow configuration source order. A disabled agent is omitted
from the roster and cannot be selected or executed. Primary-only agents cannot
be spawned as children, and subagent-only agents cannot run as primary sessions.
`all` permits both uses. Project overrides cannot re-enable a disabled lower
layer, increase its step cap, widen its tools or widen its primary/child mode.
Incompatible primary-only and child-only modes refuse the conflicting source; change its mode to the inherited mode or `all`. Forking a
primary session keeps it primary; a recorded parent is not itself a delegation.
A prompt's `maxSteps` and the agent's `steps` use the stricter bound, including
through retries and steers.

Discovery is bounded to 2048 entries, 64 definitions, 64 KiB per file and 1 MiB
in total. Invalid or unreadable definitions report an error. They are never
silently treated as absent. Agent directories are permission authority paths,
so tool edits to them require the existing owner approval.

Definitions are captured at execution admission. Prompt guidance uses the
existing session instruction epoch: an unchanged catalog preserves its prefix;
a later execution observes definition edits as ordinary instruction updates.

## Child history and worktrees

`spawn_agent` accepts `fork_turns` as a string. Omit it or use `"all"` to copy
eligible parent conversation history captured before the spawning tool call.
Use `"none"` to start with the child's task without that copied conversation.
Positive count strings such as `"3"` are accepted, but currently enable the
same history copy as `"all"`; they do not select the last three turns. Zero,
negative counts and other strings are refused.

`worktree` defaults to `false`, sharing the parent's working directory.
`worktree: true` creates a detached checkout at committed HEAD; uncommitted
parent edits stay in the parent's checkout. History copying and workspace
placement are separate choices, and both retain ancestor permission ceilings.
