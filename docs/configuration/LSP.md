# Language-server configuration

The `lsp` setting in `grapt.json` or `grapt.jsonc` selects installed language
servers for diagnostics after edits. Built-in declarations are disabled until
user-owned configuration explicitly enables them. Installed servers can execute
project build scripts, proc macros, plugins, or project-selected toolchains;
availability alone does not authorize that execution. Grapt does not download
or install language servers or their dependencies. A declaration is not proof
that its executable is available.

## Built-ins

| Name | Command (literal argv) | Extensions | Language IDs |
| --- | --- | --- | --- |
| `rust-analyzer` | `rust-analyzer` | `.rs` | `rust` |
| `gopls` | `gopls` | `.go` | `go` |
| `typescript-language-server` | `typescript-language-server --stdio` | `.ts`, `.tsx`, `.js`, `.jsx` | `typescript`, `typescriptreact`, `javascript`, `javascriptreact` |
| `pyright-langserver` | `pyright-langserver --stdio` | `.py` | `python` |
| `clangd` | `clangd` | `.c`, `.h`, `.C`, `.cc`, `.cpp`, `.cxx`, `.hh`, `.hpp`, `.hxx` | `c` for `.c`/`.h`, otherwise `cpp` |
| `zls` | `zls` | `.zig` | `zig` |

Other extensions use the final extension without its leading dot as the language
ID; files without a UTF-8 extension use an empty language ID. Extension bindings
and language IDs are separate: overriding a binding does not change the mapping.

## User configuration

```jsonc
{
  "lsp": {
    "rust-analyzer": {
      "disabled": false,
      "initialization": { "check": { "command": "clippy" } }
    },
    "custom": {
      "command": ["custom-language-server", "--stdio"],
      "extensions": [".custom"],
      "env": { "SERVER_LOG": "warn" },
      "initialization": {}
    }
  }
}
```

Only user-owned configuration may declare or override `command`, `extensions`,
`disabled`, `env`, and `initialization`. Unknown fields are refused. `command` is
literal argv, not shell text, and must include an executable. There is no `$FILE`
substitution. Custom servers need a command and extension bindings to be useful.
Initialization options must be a JSON object and are replaced, not deep-merged.
Their serialized JSON must be at most 16 KiB; omitted options default to `{}`.
Do not put credentials in configuration examples or share private environment values.

`"lsp": false` clears the registry and disables all servers. `"lsp": true` resets
the registry and explicitly authorizes all built-ins, including their ability to
execute project-controlled code. Prefer enabling individual servers with
`"disabled": false` when you do not want to grant that authority to every built-in.
An object merges supplied fields by server name; an object after a cleared registry
restores the disabled built-ins first. Later user
documents override supplied fields, while `env` merges keys (case-insensitively on
Windows). To disable one server, set its `disabled` field to `true`.

The registry holds at most 32 names, including disabled entries. Names have
1–128 bytes and no control characters. Commands have at most 128 arguments and
8 KiB of combined argument text. String-array elements must be nonempty, have no
NUL, and fit within 8 KiB. Extensions have at most 64 entries, each a dot-prefixed
suffix of at most 64 bytes without path separators. Environment keys must be
nonempty, at most 256 bytes, and contain neither `=` nor NUL; values have at most
32 KiB and no NUL. The merged environment has at most 128 keys and 64 KiB of
combined key/value text. Invalid declarations are refused with their source
configuration document identified.

## Trust boundary

Repository configuration can only narrow the user registry. It may set
`"lsp": false` or name servers with exactly `{ "disabled": true }`. It cannot
supply commands, environment, initialization options, extension bindings, or
re-enable a server. These restrictions apply after all user-owned declarations,
regardless of document ordering. Disabling an unknown name does not add it.

A language server is executable code, not a data-only parser. Explicitly enabling
a built-in or selecting a command in user-owned configuration authorizes that
server to execute project-controlled code as well as inspect files. For example,
rust-analyzer can run build scripts and proc macros and honor project toolchain
overrides. This authority is not restricted to the top-level server executable.
Process containment and external-only PATH lookup do not sandbox project code or
make repository content safe. Enable servers only for repositories you trust;
repository configuration may still disable them but cannot grant this authority.

## Tool results and process lifetime

Successful `edit`, `write`, `patch`, and `apply_patch` results include a
`diagnostics` array when an enabled server matches a published file. Diagnostics
observe the final formatted bytes, not a temporary formatter copy. They run only
after edit and undo receipts have completed, outside the publication owner. The
same server-side result reaches the CLI, TUI, and provider conversation.

Each matching server gets a contained, per-call stdio session: initialize, open
published documents, collect `publishDiagnostics`, then shutdown and exit. This
first slice deliberately does not retain idle servers or document state between
tool calls. Versions increase within the service process; stale or future
versioned notifications do not count as a result for the current text. A result
without a version is explicitly marked `versioned: false`. Files that changed
while diagnostics ran are reported as unavailable.

An observed empty notification is `status: "clean"`; nonempty diagnostics are
`status: "reported"`. Neither a missing server nor a silent, invalid, cancelled,
or timed-out exchange proves a file is clean: these report `status:
"unavailable"` and a reason. The edit remains successful and must not be retried
solely to obtain diagnostics. Deleted files are not opened.

A call checks at most 32 files/4 MiB and starts at most four servers. Its shared
exchange budget is ten seconds, with at most eight seconds for one server and
two additional seconds per process for bounded cleanup. Input framing and stderr
are bounded; diagnostic output over the feedback bound is unavailable rather
than silently truncated. Server requests to apply edits are refused; diagnostics
cannot grant new tool authority. Process containment is not a filesystem or
network sandbox.

Configured-command PATH entries are canonical, absolute directories outside the
workspace, including for interpreter/helper lookup. Windows also disables implicit
current-directory command lookup and recognizes installed `.exe`, `.cmd`, and
`.bat` entry points. User-owned absolute commands remain explicit authority.
