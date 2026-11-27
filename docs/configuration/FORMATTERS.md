# Format after edits

Formatting is opt-in in user-owned `grapt.jsonc` configuration. It includes
formatted content in `edit`, `write`, `patch` and `apply_patch` results, once per
surviving file rather than once per patch hunk. It stages planned bytes before
publication; stale edits are still refused, even if a formatter command has
already run. Read-only tools do not format.

```jsonc
{
  "formatter": {
    "source": {
      "command": ["/absolute/path/to/formatter", "--write", "$FILE"],
      "extensions": [".ts", ".tsx"],
      "environment": { "FORMAT_STYLE": "compact" },
      "disabled": false
    }
  }
}
```

`command` is literal argv, not shell text. An absolute executable or a name from
absolute PATH directories outside the workspace is supported. PATH lookup binds
execution to the resolved executable image, not a mutable alias chain. On Unix,
the requested dispatch name is carried separately. Interpreter and helper lookups
also receive a canonical, external-only PATH. Windows disables implicit
current-directory lookup and recognizes `.exe`, `.cmd`, and `.bat` entry points.
On Windows, a PATH alias
that changes the executable basename needs an explicit absolute user command. `$FILE` is replaced
inside arguments; when absent, the path is appended. Use an explicit shell in
user configuration if a command needs shell syntax. Relative script arguments
resolve from the workspace root. No formatter is installed automatically.

`false` disables all formatting. `true` enables the installed `gofmt` and `rustfmt`
built-ins. An object enables those built-ins and adds or overrides named entries.
Later user documents merge supplied fields per name, including environment keys
(case-insensitively on Windows); a boolean resets earlier entries. An entry can override a built-in's extensions or
command, or disable it. Extensions match filename suffixes, including `.d.ts`.
Custom entries need both command and extensions to run.
Absent `formatter` does not run any command. Repository documents may only disable
all formatting or set a named entry's `disabled` to `true`; they cannot enable,
replace commands, inject environment values, or add extension bindings.

Commands receive a temporary copy of the planned edited bytes with the same
extension, beside the target in the same directory, not the live file. Formatting
finishes before live mutation; publication then revalidates the original edit and
commits its formatted result and undo receipt without an asynchronous gap.
Commands see existing companion files, not other planned edits. Formatters must
support one explicit file. Successful UTF-8 output up to
1 MiB replaces the edited file through the existing verified file-write owner;
a concurrent edit is never overwritten. The result records one original-to-final
diff per file, final byte counts, and formatter success or failure. Undo includes
both edits and formatting. Formatter failure, timeout and invalid output publish
the unformatted edit instead. Interrupting before publication leaves the original
files unchanged. Command output is bounded to 16 KiB per stream and is not
included in feedback; errors identify the failure without exposing environment
values. Each command has 10 seconds, with 32 attempts and 30 seconds per tool call.
The ordinary credential/launcher filters apply after formatter environment values.

## Initial built-in scope

This slice supports installed `gofmt` and `rustfmt` only. Other languages use
explicit user commands; package discovery, automatic installation and additional
built-in registries are intentionally outside this slice. Commands that infer
behavior from the exact original basename, generate companion files or require
stdin need an explicit wrapper suitable for a disposable sibling copy. Formatter
process lifetime containment is not a sandbox for a user-authorized executable.
