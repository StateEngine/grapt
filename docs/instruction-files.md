# Instruction files

Grapt loads one instruction file per directory, in this order:

1. `AGENTS.override.md`
2. `AGENTS.md`
3. `CLAUDE.md`, only when neither AGENTS file exists

An empty file still owns its directory. An unreadable, oversized, malformed-UTF-8
or unresolved source is reported as unavailable; it never silently exposes the
next fallback. Existing last-known instruction behavior is retained.

The same selection applies to ambient instructions and instructions attached to
file reads. A more-specific directory takes precedence over its parents, project
instructions take precedence over global configuration, and conversation
instructions take precedence over instruction files. Project discovery stays
inside the repository/session boundary, with existing byte bounds and link
containment checks.

When a directory already contains an AGENTS file, edits to `CLAUDE.md` there do
not affect its loaded instructions. Remove the earlier file to select the
fallback; an empty earlier file is not absence.
