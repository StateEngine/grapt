# Skills

Grapt discovers file-backed skills in the configured skill roots, including
`.agents/skills/<id>/SKILL.md`, `.claude/skills/<id>/SKILL.md`, and
`.grapt/skills/<id>/SKILL.md`. The directory name is the skill ID and slash
command name; `name` is its display label. Use letters, numbers, dots, hyphens or
underscores in the ID.

```markdown
---
name: Release notes
description: Draft release notes from the change log
argument-hint: version
user-invocable: true
disable-model-invocation: true
---
Draft notes for $ARGUMENTS using template.md beside this skill.
```

Type `/release-notes 0.1` to open the skill's command preview. Review its source
and expanded prompt, then edit or submit it as with other workspace commands.
Skills appear in `/` completion and `/commands`. New, edited and removed skills
refresh completion automatically after a short debounce, in both project and user
skill folders. If the OS watcher is unavailable or its directory limit is reached,
the catalog shows a warning. Reopening `/` completion or using `/commands`
rescans the files. The model rereads the
skill when activating it.

`$ARGUMENTS` receives the arguments through the existing bounded, single-pass command expander; arguments
are appended when the body has no argument placeholder. The prompt identifies
the skill and its base directory for relative files.

- `user-invocable` defaults to true. Set it to false to omit the slash command
  without hiding the skill from the model.
- `disable-model-invocation` defaults to false. Set it to true to remove the
  skill from model discovery and refuse model activation through the skill tool.
  A user-invoked skill can still load individual sibling resources.
- `argument-hint` is appended to the description shown in completion.
- Standard invocation fields take precedence over the legacy `slash` and
  `metadata.grapt/slash` / `metadata.grapt/autoinvoke` aliases.
- Skills written before the rename may still use the keys
  `metadata.secode/slash` / `metadata.secode/autoinvoke`; they are read only <!-- grapt-rename: keep -->
  when the matching `grapt/` key is absent.

Existing custom commands win a name collision. Reserved built-in names retain
their meaning; colliding skills use the existing `user:` or `project:` command
namespace. File-backed skills retain command provenance and repository-profile
trust checks. A skill does not gain permission or tool grants from frontmatter.
Malformed invocation fields are reported rather than silently ignored.

Remote URLs in `skills` are not downloaded yet. Each remote source produces an
unavailable-source warning in skill discovery, the skill catalog and command
inspection, while local and plugin skills remain usable. Install the remote
skill's files in a local skill directory and configure that directory's path.
