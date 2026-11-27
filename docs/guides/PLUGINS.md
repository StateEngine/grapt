# Claude plugins

Install a Claude plugin from a local or Git marketplace without changing its files:

```sh
grapt plugin marketplace add anthropics/claude-plugins-official
# Or: grapt plugin marketplace add /path/to/marketplace
grapt plugin install plugin-name@marketplace-name
```

The CLI uses the default user profile. Use `--service /path/to/profile` to choose
another profile, or `--directory /path/to/project` to resolve relative sources
from another workspace. It starts a temporary product service when one is needed.
Installation does not require a model connection.

Inside the terminal, use `/plugin marketplace add <source>` and
`/plugin install <name@marketplace>`. Quote paths containing spaces. Installed
skills and prompt commands appear as `/plugin-name:command`. Commands use the
ordinary preview and confirmation flow. Skill resources keep their original
relative paths. Plugin root, data and project path references resolve in prompt
bodies; plugin data lives under the Grapt user configuration directory.
Claude prompt arguments use `$0` for the first argument or `$ARGUMENTS[0]`,
and `$ARGUMENTS` for the full input. The skill tool accepts an `arguments`
string when loading a plugin skill.

Marketplace sources accept local directories, GitHub `owner/repo` shorthand and
HTTPS Git URLs. Add `#ref` to select a branch or tag. Installation records the
resolved commit, so relative plugin sources use the same marketplace revision.
Plugin entries also support GitHub, HTTPS Git and Git subdirectory source objects,
including declared commit pins. Only the catalog and selected plugin enter the
installed cache; Git uses private scratch with bounded acquisition time and size.
HTTPS acquisition currently supports Linux and Windows, requires Git on PATH,
and does not prompt for remote credentials. Other platforms refuse remote acquisition.
Hosted JSON catalogs, SSH, npm, archives and dynamic sources are not available yet.
Cancelled or uncertain acquisitions retain their private directories. After four
retained attempts, restart the service and remove only abandoned acquisition
directories before retrying; published plugin caches remain retained.

Ordinary prompt skills, commands, agent definitions and bounded lifecycle command hooks activate. Agents appear in
the normal agent catalog as `plugin:agent`; default subfolders add name segments,
such as `plugin:review:security`. Agent files explicitly selected by the manifest
use `plugin:agent` without folder segments. Tool allowlists and exclusions map to
the native tools and keep the ordinary inherited permission limits. Models use
the existing user-owned `agents.modelAliases` mappings; unmapped Claude model
aliases inherit the current model.

Agent name, description, model, tools, disallowedTools and color are supported.
Plugin-agent permissionMode, hooks, mcpServers and initialPrompt
fields have no effect. Other agent execution fields and unavailable tools refuse
installation until their adapters are available.

MCP/LSP declarations,
plugin executables, workflows, settings, themes and monitors are not supported yet. Install refuses those components, including implicit component directories,
so an installation cannot silently omit behavior. Specialized prompt execution
fields are refused too. Adding a marketplace can still list plugins that a later
slice will support.

Hook files require a top-level `hooks` object. `hooks/hooks.json` merges with declared
JSON files and inline event maps. Hooks run through the native lifecycle planner,
after mandatory user hooks and before project additions. Each child receives
`CLAUDE_PLUGIN_ROOT`, `CLAUDE_PLUGIN_DATA` and `CLAUDE_PROJECT_DIR`; use double quotes
around shell path variables. Paths stay in the child environment rather than being
inserted as shell source. Single-quoted path placeholders refuse activation.
On Windows, plugin commands use Git Bash from a standard Git installation when
available, otherwise Windows PowerShell. Native user hooks keep their existing shell.

Command hooks retain the native metadata, decision, permission and timeout contract:
at most 16 commands per event, 60 seconds per command, and bounded input/output.
Tool events require a Claude tool payload adapter and refuse activation in this slice.
Non-empty lifecycle matchers, async hooks, exec-form `args`, alternate `shell` declarations and
non-command handlers refuse activation. Hook configuration uses the catalog's 64 KiB
limit. Prompt text, transcripts and provider credentials are absent from hook input.
Claude tool input/output schema adapters and additional lifecycle payload fields remain
follow-up compatibility work; this slice does not claim the full upstream hook contract.

Install is idempotent. An existing name with changed contents requires update
support, which is not available yet. Project/local installation scopes, update,
uninstall and cache cleanup are also unavailable. Published trees are retained,
including after a failed configuration publication, to protect existing sessions.
A changed installed tree refuses activation.

For migration of standalone Claude instructions, skills, hooks or MCP setup,
see [Import](IMPORT.md). Plugin installation keeps its own records and does not
modify Claude settings.

## JavaScript bridge plugins

User-global and launch-explicit `plugins` configuration can load an installed
JavaScript entrypoint through the supervised bridge. Its source directory and a
private temporary directory are writable; other workspace paths and HTTP(S)
origins require explicit declarations:

```json
{
  "plugins": [{
    "package": "./plugins/example/index.mjs",
    "permissions": {
      "read": ["./workspace/reference"],
      "write": ["./workspace/output"],
      "network": ["https://api.example.com"]
    }
  }]
}
```

Paths must exist and resolve relative to the configuration document. Declare the
specific workspace directory; filesystem roots and wildcards are refused.
Project and discovered configuration cannot grant executable plugin authority.
Reload plugins after changing a declaration.

Use `fetch` for declared HTTP(S) origins. Origins include the scheme and port;
redirect destinations need their own declaration. Requests and responses are
buffered up to 128 KiB each (smaller when the bridge frame limit is smaller),
with a 30-second exchange deadline and at most ten redirects. Native sockets,
WebSockets, process creation, native addons and runtime escape APIs are unavailable.
`plugin_network_denied` names the missing origin; `plugin_filesystem_denied`
names the declaration to change.

OS isolation applies independently of Node permissions: Linux uses bubblewrap
with namespaces and seccomp; Windows uses native MXC process containment on
Windows 11 build 26100.9278 or newer; macOS uses Seatbelt. Existing CPU, memory,
process and teardown bounds remain active. An unavailable backend refuses startup.
