# Import prior agent setup

Inside the TUI, `/import` previews both sources in the current workspace. Use
`/import claude` or `/import codex` to preview one. Close the result pane and run
`/import apply` to import the previewed sources. A workspace change requires a
fresh preview. Import runs while the selected session is idle; input and event
delivery remain available while it reads setup.

From a shell, choose one kind:

```sh
grapt import instructions --from claude --directory /path/to/project
grapt import skills --from codex --directory /path/to/project
grapt import hooks --from claude --service /path/to/profile --directory /path/to/project
grapt import mcp --from codex --service /path/to/profile --directory /path/to/project
```

These commands preview by default. Add `--apply` to write. Hooks use the default
user profile when `--service` is omitted; MCP imports require an explicit profile.
Reports show paths, server names and hook event names, never source contents,
commands, environment values, headers or full MCP URLs.

## Instructions

Claude project instructions become an `AGENTS.md` that references the original
`CLAUDE.md`, `CLAUDE.local.md` and `.claude/CLAUDE.md` files. Relative imports keep
their original base. Scoped `.claude/rules` files become native `.grapt/rules`
files with their `paths` or `globs` frontmatter preserved. Nested rule filenames
are flattened, and relative body imports are rebased. Rules without a path scope
are referenced by the ambient instructions.

An existing `AGENTS.md` or `AGENTS.override.md` wins. The importer writes
`AGENTS.import-claude.md` beside it and reports that the existing file needs an
explicit `@AGENTS.import-claude.md` include to activate the imported instructions.
It never edits that existing file. Project `AGENTS.md` and
`AGENTS.override.md` are already used natively and are not copied.

Global instruction sources are mirrored, with only their referenced instruction
files, below the Grapt config directory. Mirrored include paths use the resolved
source names, so file and directory case variations remain valid in the new location.
Existing global instructions win too;
side files require an explicit include. Unscoped home-directory Claude rules remain global instructions; path-scoped
home rules are reported as unsupported because native scoped rules belong to a
workspace. Conflicting project rules are kept beside the existing file with an
`.import-claude` suffix and are not loaded automatically.

## Skills, MCP and agents

Project and home `.claude/skills` and `.agents/skills` roots are already discovered
natively. `.codex/skills` directories are copied to the matching `.agents/skills`
root, including scripts and references. Existing skill directories win as a unit.
Import preserves repository trust: project skills still require an approved
repository profile before they can run.
Sources are bounded regular trees; links are refused. New executable resources
retain owner execution permission. If a file write fails, inspect the reported
partial import before retrying; existing destinations are never overwritten.

MCP imports reuse the configuration-only catalog and the existing `mcp add`
writer. An existing server declaration wins, including a disabled declaration.
The import does not start MCP peers to inspect configuration.

For Claude agents, the TUI reads the product's ordinary agent roster and reports
files already offered by the native `.claude/agents` loader. It never copies or
rewrites those agent definitions.

## Hooks

Hook import reads the `hooks` block in home and project `.claude/settings.json`
and project `.claude/settings.local.json`. It honors `disableAllHooks`, preserves
matchers and uses the native hook validator. Unsupported hook types, fields or
events are refused rather than silently dropped. An existing event declaration
in Grapt wins. Only an explicit apply enables future command execution.

Imported commands run in the selected workspace with `CLAUDE_PROJECT_DIR` set.
They use the native POSIX shell or Windows PowerShell. User configuration is
written through the existing locked atomic publisher; repository profiles cannot
be promoted to user hook authority. Reopen the session to reload imported setup.

For local Claude plugin packages and marketplaces, see [Plugins](PLUGINS.md).
