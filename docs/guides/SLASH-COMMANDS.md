# Workspace slash commands

Type `/` in R to browse commands. Up and Down select, Tab completes the name,
and Escape closes the list while keeping your draft. Enter runs an exact
command name; for a partial name it completes the selected match first.

`/review` reviews uncommitted changes. Add a commit, branch or pull request to
choose another target, for example `/review main`. Review runs as a normal
agent task with your session's model and tool permissions.

R loads the existing command catalog for the attached workspace. Define prompt
commands in `grapt.json` or `.grapt/commands/<name>.md`, for example:

```json
{
  "commands": {
    "check": {
      "description": "Check the requested files",
      "template": "Check $1 and $2 for correctness."
    }
  }
}
```

Run `/check "src/file one.rs" src/two.rs`. `$1` through `$9` use quoted argument
groups, with the last numbered placeholder keeping any remaining arguments.
`$ARGUMENTS` inserts the complete argument text. Substitutions happen
once. Templates without placeholders receive an Arguments paragraph. Expanded
prompts are limited to 64 KiB.

`/commands` lists the workspace commands and reloads the catalog. Built-in
commands keep their existing meanings. R preserves drafts when loading fails,
arguments are missing or expansion is too large. Specialized templates that
select an agent or model, start a subtask or interpolate shell commands are
reported as unavailable in R. Unknown slash text retains its ordinary prompt
behavior after the catalog loads.
