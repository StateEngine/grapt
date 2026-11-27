# Configuration sources

Every part of Grapt that reads configuration (commands, skills, references,
MCP servers, plugins, compaction) reads the same
documents, in this order. A later document wins per key.

1. The profile's own `opencode.jsonc` (`<service root>/opencode.jsonc`, which
   first-run writes).
2. Your user config directory (`OPENCODE_CONFIG_DIR`, else
   `$XDG_CONFIG_HOME/opencode`, else `~/.config/opencode`).
3. `OPENCODE_CONFIG`.
4. Each project ancestor's `opencode.json(c)`, outermost first, then each
   `.opencode` directory.
5. `OPENCODE_CONFIG_CONTENT`.

A document that cannot be read is refused by every reader. MCP no longer
publishes no tools silently in that case.

## Yours, or a repository's

Documents 1-3 and 5 are yours. Project documents (4) come with a repository
you may have just cloned, so they may only make Grapt more careful:

- A project's **local MCP servers** (`"type": "local"`, a command) are not
  started. The Session shows: `MCP unavailable this turn: <name> (local
  servers from project config are blocked; add it to your user config to
  allow it)`. Its remote servers still connect.
- A project's **plugins** never load. They are listed with status `blocked`
  and the reason.

Only the profile's own `opencode.jsonc` is yours. A repository that happens
to contain a service root, as test and bench roots do, stays a repository.

## Changed on upgrade

- The profile's `opencode.jsonc` is now read by every reader, including
  commands, skills, references and MCP for a project outside the service
  root. Before, only compaction read it there.
- **Plugins declared in the profile's own `opencode.jsonc` now load.**
  Before, the plugin reader took that file for a project document and
  blocked them.
- A project's local MCP servers no longer start.
