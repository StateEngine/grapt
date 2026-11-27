# SE Code parity audit — November 2026

Audit date: 2026-11-04. Independently spot-checked.
Requested scope: Codex, Claude Code, OpenCode, with Hermes Agent as an additional
reference. Anthropic subscription OAuth is explicitly excluded; Anthropic API-key
support is included. This is research and ticket preparation, not permission to
implement the backlog or expand the 0.1 release gate.

## Baseline and interpretation

SE Code baseline is landed Windows `<checkout>` `dev`,
`9ea315cdb1ff3c78d7c0b96be6b8d706183750e0`, fetched on 2026-11-04.
All SE file:line references below are relative to that commit. The primary
interactive product is `secode-tui`, launched through
`crates/secode-cli/src/command/terminal.rs:1`; `secode-tui` also supplies shared
client/protocol code. Its smaller legacy command dispatcher is not the product
inventory. Product code was inspected without making provider calls or changing
credentials, configuration, databases, or product source.

Competitor documentation was retrieved on the audit date. OpenCode source docs
are pinned to `anomalyco/opencode` commit
`b3fd4a75fe539a65168139cec2edd29ceb7724f6`; Hermes source docs to
`NousResearch/hermes-agent` commit `71736c9f32597bbca9d89cede5aa136e55946b2f`.
Claude Code observations use public documentation only. No proprietary source was
read. Documentation establishes advertised behavior, not measured quality.

Status meanings: **present** means an ordinary product path exists; **partial**
names a specific narrower behavior; **gap** means the inspected path and landed
history do not supply the advertised capability; **unverified** is not an absence
claim. A generic shell or configurable MCP server is an extension mechanism, not
proof of a bundled competitor feature. Conversely, different names or architecture
do not create a gap when the same user outcome already works.

`docs/RELEASE-0.1.md` remains the release authority. Its still-open matched-task,
named-tool-failure and headless-seat criteria are acceptance work, not evidence
that the corresponding feature is wholly missing.

## Feature matrix

Each competitor cell cites its own documentation. `—` means the consulted source
does not establish an equivalent feature; it does **not** mean the competitor
lacks it. Source links at the end resolve every shorthand. SE links point into
this Rust repository; their labels retain the relevant file and line.

### Tools

| ID / capability | Codex | Claude Code | OpenCode | Hermes reference | SE Code at baseline / gap |
| --- | --- | --- | --- | --- | --- |
| T01 Execute commands | Local execution [C-cli] | Bash [A-tools] | bash [O-tools] | terminal [H-tools] | **Present**: bounded shell [tools/catalog.rs:42](../../crates/secode-product/src/dogfood/tools/catalog.rs#L42). |
| T02 PTY, background processes, poll/input | Unified exec [C-prompt] | Background Bash [A-interactive] | Bash; headless server [O-cli] | process handles [H-tools] | **Present**: `exec_command`, `write_stdin`, PTY, cursors [tools/terminal.rs:50](../../crates/secode-product/src/dogfood/tools/terminal.rs#L50). Handles are process-local; recovery uses history rather than replay. |
| T03 Read/edit/write, glob/grep | Repository inspection/edit [C-cli] | Dedicated tools [A-tools] | Dedicated tools [O-tools] | File tools [H-tools] | **Present** [tools/catalog.rs:52](../../crates/secode-product/src/dogfood/tools/catalog.rs#L52), [tools/coding.rs:30](../../crates/secode-product/src/dogfood/tools/coding.rs#L30). |
| T04 Codex patch format | apply_patch [C-prompt] | Edit/Write outcome [A-tools] | apply_patch [O-tools] | patch [H-tools] | **Present**: freeform grammar and governed executor [coding/patch/apply_patch.rs:15](../../crates/secode-product/src/dogfood/tools/coding/patch/apply_patch.rs#L15). |
| T05 Search the web | Cached/live search [C-web] | Hosted search [A-tools] | Provider-gated search [O-tools] | web_search [H-tools] | **Present**, route-dependent: Responses/Codex native search; Anthropic hosted search landed `d4f9fc706`; configurable plugin fallback [dogfood/web_search.rs:1](../../crates/secode-product/src/dogfood/web_search.rs#L1). Do not ticket Anthropic search as missing. |
| T06 Fetch page content | Web context [C-web] | WebFetch [A-tools] | webfetch [O-tools] | web_extract [H-tools] | **Present**: bounded fetch [tools/catalog.rs:74](../../crates/secode-product/src/dogfood/tools/catalog.rs#L74), `crates/secode-web/src/fetch.rs:1`. Rendering a JavaScript app is a separate browser feature, T13. |
| T07 View local/prompt images | Image input [C-image] | Read images [A-tools] | Image inputs [O-config] | vision_analyze [H-tools] | **Present**: `view_image`, clipboard, `/attach`, CLI `--image` [tools/image.rs:16](../../crates/secode-product/src/dogfood/tools/image.rs#L16), [tui-r/clipboard.rs:1](../../crates/secode-tui/src/clipboard.rs#L1). |
| T08 Track a plan | Planning [C-commands] | Task tools [A-tools] | todowrite [O-tools] | todo [H-tools] | **Present**: `update_plan` and projection [tools/catalog.rs:89](../../crates/secode-product/src/dogfood/tools/catalog.rs#L89), `crates/secode-client/src/projection/plan.rs:1`. Plan *mode* is U13. |
| T09 Ask the user | Interactive prompts [C-commands] | AskUserQuestion [A-tools] | question [O-tools] | clarify [H-tools] | **Present**: `request_user_input` plus durable session forms [tools/ask.rs:1](../../crates/secode-product/src/dogfood/tools/ask.rs#L1), [grapt-tui-forms/ask.rs:1](../../crates/grapt-tui-forms/src/ask.rs#L1). |
| T10 Compose tool calls in code | Programmatic workflows [C-prompt] | Tool batching [A-tools] | Plugin/custom tools [O-custom] | execute_code [H-tools] | **Present**: CodeMode worker through ordinary tool authority [tools/codemode.rs:1](../../crates/secode-product/src/dogfood/tools/codemode.rs#L1). Language/runtime spelling is not a parity defect. |
| T11 Notebook cell editing | — | NotebookEdit [A-tools] | Generic file tools [O-tools] | Generic file tools [H-tools] | **Gap PAR-06**: catalog has no cell-aware notebook adapter [tools/catalog.rs:4](../../crates/secode-product/src/dogfood/tools/catalog.rs#L4). Raw JSON editing is possible. |
| T12 Code intelligence / automatic diagnostics | Editor integrations [C-ide] | Plugin-backed LSP [A-tools] | Opt-in LSP [O-lsp] | Native diagnostics [H-cli] | **Gap PAR-05**: text search exists, native language-server lifecycle and diagnostic feedback do not [tools/catalog.rs:4](../../crates/secode-product/src/dogfood/tools/catalog.rs#L4). |
| T13 Interactive browser automation | Plugin extension [C-plugins] | Chrome integration [A-chrome] | Custom/MCP extension [O-custom] | CDP browser tools [H-tools] | **Partial / PAR-28**: arbitrary MCP tools can provide this; no bundled browser connection/workflow in the catalog [tools/catalog.rs:4](../../crates/secode-product/src/dogfood/tools/catalog.rs#L4). |
| T14 Image generation, speech input/output | Plugin extension [C-plugins] | Voice command [A-commands] | Extension mechanism [O-custom] | image_generate, TTS, voice [H-tools] [H-voice] | **Partial / PAR-29**: image *viewing* exists; no first-party generation/audio workflow [tools/catalog.rs:4](../../crates/secode-product/src/dogfood/tools/catalog.rs#L4). |
| T15 Format after edits | Hook extension [C-hooks] | Hook/plugin extension [A-hooks] | Opt-in formatter registry [O-format] | Plugin extension [H-plugins] | **Partial / PAR-07**: shell/hooks can format; no language-specific formatter configuration and edit feedback integration [tools/executor.rs:117](../../crates/secode-product/src/dogfood/tools/executor.rs#L117). |
| T16 Read PDF pages | — | PDF page-range read [A-tools] | Document attachment [O2-attachments] | Vision/integrations [H-tools] | **Partial / PAR-55**: local viewing is raster-only and file reads are UTF-8 [tools/image.rs:23](../../crates/secode-product/src/dogfood/tools/image.rs#L23), [tools/catalog.rs:41](../../crates/secode-product/src/dogfood/tools/catalog.rs#L41). Shell conversion/MCP are user workarounds. |
| T17 Non-UTF-8 text and rendered notebook reading | Shell/file context [C-cli] | UTF-16LE and cell/output reads [A-tools] | File tools [O-tools] | File tools [H-tools] | **Partial / PAR-56/PAR-06b**: native read/edit are UTF-8 and notebooks are raw JSON; inspect [tools/effects.rs:50](../../crates/secode-product/src/dogfood/tools/effects.rs#L50). |

### Terminal interactions and slash commands

| ID / capability | Codex | Claude Code | OpenCode | Hermes reference | SE Code at baseline / gap |
| --- | --- | --- | --- | --- | --- |
| U01 Multiline edit, paste, recall | Composer [C-commands] | Editor/history [A-interactive] | Editor/keybinds [O-keys] | CLI controls [H-cli] | **Present**: bounded editor, paste handling, recall [tui-r/editor.rs:1](../../crates/secode-tui/src/editor.rs#L1), `crates/secode-tui/src/recall.rs:1`. |
| U02 File mentions and completion | File context [C-commands] | `@` [A-interactive] | `@` file search [O-tui] | Context files [H-context] | **Present**: workspace finder [tui-r/mentions.rs:1](../../crates/secode-tui/src/mentions.rs#L1). |
| U03 Command palette, shortcut configuration | CLI controls [C-commands] | Keybindings [A-interactive] | Leader keys [O-keys] | CLI controls [H-cli] | **Present**: palette and configurable native actions/chords, landed `7dd68fb7c` [tui-r/actions.rs:11](../../crates/secode-tui/src/actions.rs#L11), `crates/secode-tui/src/keybindings/config.rs:41`. This is native-action rebinding, not every text-editing key. |
| U04 Settings, theme, notices | Theme/config [C-custom] | `/config`, `/theme` [A-commands] | themes [O-themes] | Configuration [H-config] | **Present** for saved light/dark, notification, copy and wheel preferences, landed `7f0f56076` [tui-r/settings.rs:16](../../crates/secode-tui/src/settings.rs#L16). **Custom palettes gap PAR-10** [tui-r/palette.rs:16](../../crates/secode-tui/src/palette.rs#L16). |
| U05 External prompt editor | VISUAL/EDITOR [C-custom] | Ctrl-G [A-interactive] | `/editor` [O-tui] | — | **Gap PAR-02**: no native editor action [tui-r/actions.rs:11](../../crates/secode-tui/src/actions.rs#L11). |
| U06 Direct user shell command | `!` [C-commands] | `!` [A-interactive] | `!` [O-tui] | — | **Gap PAR-08**: no equivalent dispatch in [tui-r/backend/commands.rs:160](../../crates/secode-tui/src/backend/commands.rs#L160). Agent shell tools exist; this is a user interaction gap. |
| U07 Vim composer mode | — | `/vim` [A-interactive] | Editor key customization [O-keys] | Vim config [H-config] | **Gap PAR-09**: current composer is ordinary editing without a modal Vim state [tui-r/editor.rs:30](../../crates/secode-tui/src/editor.rs#L30). |
| U08 Inline diffs, readable tool rows, syntax | Terminal diff [C-custom] | Diff/transcript [A-interactive] | Tool details [O-tui] | Tool displays [H-cli] | **Present**: folded inline file diffs landed `5d478f1de`; highlighting/transcript [tui-r/transcript.rs:1](../../crates/secode-tui/src/transcript.rs#L1), `crates/secode-tui/src/highlight.rs:1`. |
| U09 Interrupt, steer, queue and follow-ups | Steer active work [C-cli] | Queued messages [A-interactive] | Interactive work [O-tui] | Interrupt/new message [H-cli] | **Present**: Esc, retained drafts and follow-ups [tui-r/keys.rs:1](../../crates/secode-tui/src/keys.rs#L1), `crates/secode-tui/src/followups.rs:1`. No claim of measured equivalence for every queue race. |
| U10 Basic control commands | new/resume/model/status [C-commands] | Same outcomes [A-commands] | Same outcomes [O-tui] | new/model/usage [H-cli] | **Present**: `/new`, `/clear`, `/resume`, `/rename`, `/model`, `/effort`, `/status`, `/help`, `/quit`, aliases [tui-r/slash.rs:50](../../crates/secode-tui/src/slash.rs#L50), [tui-r/actions.rs:139](../../crates/secode-tui/src/actions.rs#L139). |
| U11 Work/context commands | init/review/compact/diff [C-commands] | Same outcomes [A-commands] | init/compact/export [O-tui] | compress/skills [H-cli] | **Present**: `/init`, `/review`, `/compact`, `/diff`, `/context`, `/export`, `/commands`, `/import`, `/mcp`, `/connect` [tui-r/slash.rs:50](../../crates/secode-tui/src/slash.rs#L50). No ticket merely for an alternate alias. |
| U12 Side question and branch | Fork [C-commands] | btw/branch [A-commands] | Session branching [O-cli] | Delegation [H-delegate] | **Present**: `/side`, `/fork`, `/worktree` [tui-r/backend/commands.rs:416](../../crates/secode-tui/src/backend/commands.rs#L416). |
| U13 Dedicated plan interaction | Plan mode [C-commands] | `/plan` [A-commands] | Plan primary agent [O-agents] | todo/planning [H-tools] | **Partial / PAR-37**: planning tool, explorer and permission presets exist; `/plan` is reserved but not dispatched [dogfood/catalog_commands.rs:33](../../crates/secode-product/src/dogfood/catalog_commands.rs#L33), [tui-r/slash.rs:50](../../crates/secode-tui/src/slash.rs#L50). |

### Sessions, agents and recovery

| ID / capability | Codex | Claude Code | OpenCode | Hermes reference | SE Code at baseline / gap |
| --- | --- | --- | --- | --- | --- |
| S01 Save/resume/rename/export | Resume/fork [C-commands] | Session management [A-sessions] | Session CLI/export [O-cli] | Session management [H-cli] | **Present**: native commands and API clients [tui-r/backend/commands.rs:160](../../crates/secode-tui/src/backend/commands.rs#L160), `crates/secode-client/src/client/export.rs:1`. |
| S02 Background headless session | exec/cloud workflows [C-exec] [C-cloud] | Background agents [A-cli] | run/attach/server [O-cli] | Background processes [H-tools] | **Present**: `run --background`, `--resident`, attach [cli/command/help.rs:143](../../crates/secode-cli/src/command/help.rs#L143). StateEngine headless acceptance remains open in release criterion 7; feature existence does not close it. |
| S03 Isolated worktrees | Worktree flag [C-commands] | worktree isolation [A-cli] | Fork/session workflows [O-cli] | Worktree mode [H-config] | **Present**: CLI creation and native create/open/merge/remove, landed `7413d7531` [tui-r/slash.rs:79](../../crates/secode-tui/src/slash.rs#L79), `crates/secode-worktree/src/lib.rs:1`. |
| S04 Parallel subagents and controls | Agent workflows [C-agents] | Subagents [A-agents] | task/subagents [O-agents] | delegate_task [H-delegate] | **Present**: spawn/list/wait/interrupt/message/follow-up, inherited context and authority [tools/catalog.rs:78](../../crates/secode-product/src/dogfood/tools/catalog.rs#L78), `crates/secode-subagent/src/lib.rs:1`. |
| S05 Custom agent definitions | Configured agents [C-agents] | Markdown definitions [A-agents] | JSON/Markdown [O-agents] | Delegation roles [H-delegate] | **Present**, subset of metadata: native definitions and `.claude/agents` name/description/model/tool ceilings, landed `f1777820c` [agent_definitions/claude.rs:1](../../crates/secode-product/src/dogfood/agent_definitions/claude.rs#L1). Skill-level fork execution is P03, not this feature. |
| S06 Conversation rewind | Fork/undo controls [C-commands] | Rewind conversation [A-checkpoint] | undo/redo [O-tui] | `/undo`, `/retry` [H-cli] | **Present**: preview and rewind with reopened draft [tui-r/backend/commands.rs:465](../../crates/secode-tui/src/backend/commands.rs#L465), [dogfood/undo_route.rs:44](../../crates/secode-product/src/dogfood/undo_route.rs#L44). |
| S07 File checkpoint coverage | Git checkpoints recommended [C-cli] | Edit snapshots [A-checkpoint] | Snapshot/undo [O-config] [O-tui] | Undo [H-cli] | **Partial / PAR-11a**: automatic patch checkpoints exist; ordinary `edit` and `write` take non-checkpoint paths [tools/executor.rs:117](../../crates/secode-product/src/dogfood/tools/executor.rs#L117). `/undo` must not be advertised as covering all file tools. |
| S08 Redo / choose earlier file restore point | — | Prompt checkpoint chooser [A-checkpoint] | `/redo` [O-tui] | `/retry` is regeneration [H-cli] | **Gap PAR-11b**: native undo previews the selected patch group; no redo or earlier-file-checkpoint chooser [dogfood/undo_route.rs:124](../../crates/secode-product/src/dogfood/undo_route.rs#L124), [tui-r/slash.rs:50](../../crates/secode-tui/src/slash.rs#L50). Composer text undo/redo already exists. |
| S09 Shared team tasks/dependencies | Parallel agents [C-agents] | Agent teams/shared tasks [A-teams] | Subagent delegation [O-agents] | Delegation [H-delegate] | **Partial / PAR-32**: per-parent child controls exist; no ordinary shared task board/dependency assignment interface [tools/agents.rs:11](../../crates/secode-product/src/dogfood/tools/agents.rs#L11). Parked Campaign Mode is not shipped parity. |
| S10 Cross-session recall / search | Memory configuration [C-config] | Session search/auto memory [A-memory] | Session selection [O-cli] | session_search [H-memory] | **Partial / PAR-21**: `read_history` reads the current session, not a user-authorized cross-session search [tools/read_history.rs:6](../../crates/secode-product/src/dogfood/tools/read_history.rs#L6). |
| S11 Portable session import | — | — | Import/export [O-cli] | Session tools [H-cli] | **Partial / PAR-13c**: `/import` migrates setup, not transcripts; `secode-migrate` is the legacy DB path [cli/migration.rs:1](../../crates/secode-cli/src/migration.rs#L1), [cli/compatibility.rs:42](../../crates/secode-cli/src/compatibility.rs#L42). Native JSON import needs its own explicit identity/provenance contract. |

### Sandbox and permission control

| ID / capability | Codex | Claude Code | OpenCode | Hermes reference | SE Code at baseline / gap |
| --- | --- | --- | --- | --- | --- |
| X01 Tool/path permissions and presets | Sandbox/approval policy [C-security] | Rules/modes [A-permissions] | allow/ask/deny patterns [O-permissions] | Approval policy [H-config] | **Present**: inspect/author/maintainer/bypass and narrower descendants [dogfood/permission_preset.rs:1](../../crates/secode-product/src/dogfood/permission_preset.rs#L1). Exact enum names need not match. |
| X02 Native isolation | OS sandbox [C-security] | OS containment [A-sandbox] | Permission policy [O-permissions] | Backend isolation [H-tools] | **Present**: Linux bwrap, Windows containment, macOS Seatbelt [tools/sandbox/posix.rs:1](../../crates/secode-product/src/dogfood/tools/sandbox/posix.rs#L1), `crates/secode-sandbox/src/lib.rs:1`; macOS landed `6cbaf7627`, Linux activation `573aa3c30`. Platform operation was not requalified in this research. |
| X03 Grant preview, retry, withdrawal | Writable-root controls [C-security] | Sandbox settings [A-sandbox] | Permission controls [O-permissions] | Approval controls [H-config] | **Present**: exact contained retry and grant withdrawal landed `fc3d1660a`; command-only Windows MSYS consent is explicit [tools/sandbox_access.rs:1](../../crates/secode-product/src/dogfood/tools/sandbox_access.rs#L1), [tui-r/slash.rs:63](../../crates/secode-tui/src/slash.rs#L63). |
| X04 Credentials / shell environment | Auth/security [C-security] | Settings/security [A-settings] | Provider auth [O-providers] | Environment settings [H-config] | **Present**: connection/vault ownership and shell credential filtering [tools/shell_environment.rs:1](../../crates/secode-product/src/dogfood/tools/shell_environment.rs#L1), `crates/secode-connections/src/lib.rs:1`. Anthropic subscription sign-in is excluded, not a gap. |

### MCP

| ID / capability | Codex | Claude Code | OpenCode | Hermes reference | SE Code at baseline / gap |
| --- | --- | --- | --- | --- | --- |
| M01 Local stdio and remote HTTP | Both [C-mcp] | Both [A-mcp] | Local/remote [O-mcp] | Both [H-mcp] | **Present**: ordinary local/remote registry [dogfood/mcp/local.rs:1](../../crates/secode-product/src/dogfood/mcp/local.rs#L1), [dogfood/mcp/remote.rs:1](../../crates/secode-product/src/dogfood/mcp/remote.rs#L1). Legacy HTTP+SSE endpoint interoperability is **unverified**, not inferred from the SSE frame decoder. |
| M02 Remote sign-in / reconnect | OAuth [C-mcp] | OAuth [A-mcp] | OAuth [O-mcp] | OAuth [H-mcp] | **Present**: auth owner and TUI/CLI sign-in [dogfood/mcp/auth.rs:1](../../crates/secode-product/src/dogfood/mcp/auth.rs#L1), [cli/command/mcp.rs:1](../../crates/secode-cli/src/command/mcp.rs#L1). |
| M03 Tools, discovery, filtered exposure | Discovery/config [C-mcp] | Tool search [A-mcp] | Tools/permission rules [O-mcp] | Exposure controls [H-mcp] | **Present** for publication and withheld-tool reasons [dogfood/mcp/publication.rs:1](../../crates/secode-product/src/dogfood/mcp/publication.rs#L1). Exact lazy-discovery efficiency versus Claude remains **unverified**, not claimed equal. |
| M04 Prompts/resources/templates | Resource tools [C-mcp] | Prompts/resources [A-mcp] | Server integration [O-mcp] | MCP resource/prompt utilities [H-mcp] | **Present**: native content picker and command catalog, landed `d5ce28374` [dogfood/mcp/content.rs:127](../../crates/secode-product/src/dogfood/mcp/content.rs#L127), `crates/secode-product/src/dogfood/catalog_mcp.rs:1`. |
| M05 Resource watches | — | MCP integration [A-mcp] | MCP integration [O-mcp] | MCP events [H-mcp] | **Present**: opt-in selected resource subscriptions, no automatic context injection, landed `a7ad46868` [dogfood/mcp/content.rs:72](../../crates/secode-product/src/dogfood/mcp/content.rs#L72). |
| M06 Workspace roots/progress | MCP client [C-mcp] | MCP client [A-mcp] | MCP client [O-mcp] | MCP integration [H-mcp] | **Present**: scoped roots and progress landed `17d1b57e4` [dogfood/mcp/roots.rs:1](../../crates/secode-product/src/dogfood/mcp/roots.rs#L1). |
| M07 Elicitation | MCP client [C-mcp] | Elicitation [A-mcp] | MCP extension [O-mcp] | Form elicitation [H-mcp] | **Present**: incoming `elicitation/create` into forms [dogfood/mcp/channel.rs:417](../../crates/secode-product/src/dogfood/mcp/channel.rs#L417). URL-mode parity is **unverified**. |
| M08 Sampling | — | — | — | sampling/createMessage [H-mcp] | **Gap PAR-40**: incoming dispatch handles roots/forms, not sampling [dogfood/mcp/channel.rs:415](../../crates/secode-product/src/dogfood/mcp/channel.rs#L415). |
| M09 Expose harness as MCP server | Removed; app server replaces it [C-commands] | MCP tooling [A-mcp] | HTTP SDK/server [O-server] | Official mcp-serve plugin [H-mcp] | **Gap PAR-33** relative to Hermes: client support exists; `mcp serve` explicitly refuses [cli/compatibility.rs:38](../../crates/secode-cli/src/compatibility.rs#L38). Do not open a ticket to restore removed Codex `mcp-server` merely for naming parity. |

### Hooks, skills and plugins

| ID / capability | Codex | Claude Code | OpenCode | Hermes reference | SE Code at baseline / gap |
| --- | --- | --- | --- | --- | --- |
| H01 Lifecycle command hooks / tool decisions | Command hooks [C-hooks] | Command hooks [A-hooks] | Plugin hooks [O-plugins] | Plugin hooks [H-plugins] | **Present**: 13 named events, tool matchers, allow/ask/deny/modify and PermissionRequest decisions [guides/HOOKS.md:1](../guides/HOOKS.md#L1). Project bindings only narrow. |
| H02 Additional events and rich payloads | Request/interrupt events [C-hooks] | Failure/config/worktree events [A-hooks] | Event callbacks [O-plugins] | Event callbacks [H-plugins] | **Partial / PAR-16**: finite event enum and metadata/tool-input protocol; no full prompt/result/request-attempt payload contract [session/notice/lifecycle_hook.rs:1](../../crates/secode-session/src/notice/lifecycle_hook.rs#L1), [guides/HOOKS.md:183](../guides/HOOKS.md#L183). |
| H03 HTTP/MCP/prompt/agent hook handlers | MCP handlers [C-hooks] | HTTP/MCP/prompt/agent handlers [A-hooks] | JS/TS callbacks [O-plugins] | Python callbacks [H-plugins] | **Partial / PAR-17a–c**: inline group accepts only `type: command` [hooks/inline.rs:77](../../crates/secode-product/src/dogfood/hooks/inline.rs#L77). Plugin callbacks are a different existing path. |
| H04 Async hooks and bounded stop continuation | Stop control [C-hooks] | Async and stop blocking [A-hooks] | Lifecycle plugins [O-plugins] | Lifecycle plugins [H-plugins] | **Partial / PAR-18a–b**: command hooks are bounded foreground observers/enforcers; Stop cannot restart a response chain [guides/HOOKS.md:1](../guides/HOOKS.md#L1). |
| P01 Discover/load/invoke skills, live refresh | Skills [C-plugins] | Skills/frontmatter [A-skills] | SKILL.md [O-skills] | Skills hub/learning [H-skills] | **Present**: progressive loading, invocation controls, argument hint, slash discovery and filesystem refresh [dogfood/catalogs.rs:269](../../crates/secode-product/src/dogfood/catalogs.rs#L269), `crates/secode-product/src/dogfood/skills_watch.rs:1`. Import setup also preserves skill directories. |
| P02 Skill-local execution metadata | Agent/skill workflow [C-plugins] | context: fork, agent, hooks [A-skills] | Skills as tool [O-skills] | Bundles/learning [H-skills] | **Partial / PAR-19**: parser applies name/description/invocation/hint; it does not apply skill-local agent/tool/hook execution scope [dogfood/catalogs.rs:269](../../crates/secode-product/src/dogfood/catalogs.rs#L269). |
| P03 Custom tool/plugin execution | Plugins [C-plugins] | Plugins/MCP [A-plugin] | JS/TS/npm custom tools [O-custom] [O-plugins] | Python plugins [H-plugins] | **Present** through bounded external bridge and scoped host calls [dogfood/plugins.rs:1](../../crates/secode-product/src/dogfood/plugins.rs#L1), `crates/secode-plugin/src/protocol.rs:1`. Runtime/module-format interoperability is separate from having plugins. |
| P04 Plugin management/marketplaces | Plugin marketplace commands [C-commands] | Install/enable/marketplace [A-plugin] | npm/local plugins [O-plugins] | Catalog/install/enable [H-plugins] | **Partial / PAR-15a–b**: config declaration and live list exist; CLI plugin management refuses; no Claude/Codex plugin bundle installer [cli/compatibility.rs:50](../../crates/secode-cli/src/compatibility.rs#L50), [plugin/declaration.rs:94](../../crates/secode-plugin/src/declaration.rs#L94). |
| P05 Persistent auto memory | Memory settings [C-config] | Automatic memory files [A-memory] | Rules/config [O-config] | MEMORY.md/USER.md [H-memory] | **Partial / PAR-20**: static/scoped AGENTS/CLAUDE/rules and StateEngine MCP are available; no harness-owned automatic memory lifecycle [dogfood/instructions.rs:50](../../crates/secode-product/src/dogfood/instructions.rs#L50). |

### Providers and connections

| ID / capability | Codex | Claude Code | OpenCode | Hermes reference | SE Code at baseline / gap |
| --- | --- | --- | --- | --- | --- |
| V01 OpenAI / ChatGPT subscription | API/ChatGPT auth [C-cli] | — | Provider connections [O-providers] | Provider setup [H-config] | **Present**: Responses and Codex protocols [provider/request.rs:575](../../crates/secode-provider/src/request.rs#L575); browser/device ChatGPT sign-in landed `2ba291b10` / `5f2d86e09`. |
| V02 Anthropic API key | Custom provider config [C-config] | Native [A-third-party] | Anthropic [O-providers] | Provider setup [H-config] | **Present**: AnthropicMessages family [provider/request.rs:575](../../crates/secode-provider/src/request.rs#L575); API-key session cache criterion 9 already accepted. Subscription OAuth excluded. |
| V03 Compatible endpoints and local models | Configured gateways [C-config] | Gateways [A-third-party] | Local/compatible models [O-models] | Local/custom endpoint [H-config] | **Present**: OpenAiCompatible family and literal loopback HTTP, landed `9b4e15b54` [provider/route.rs:1](../../crates/secode-provider/src/route.rs#L1). “Local Qwen unsupported” would be a stale/overbroad claim; native noncompatible protocols are separate. |
| V04 Native Google Gemini | — | — | Google provider [O-providers] | Provider selection [H-config] | **Gap PAR-22**: no native Gemini family in closed protocol enum [provider/request.rs:575](../../crates/secode-provider/src/request.rs#L575). Gemini through a compatible gateway is already possible. |
| V05 AWS Bedrock / Google Vertex | — | Native enterprise routes [A-third-party] | Both [O-providers] | Provider setup [H-config] | **Gap PAR-23/PAR-24**: no signed Bedrock or Vertex-specific native transport/auth family [provider/request.rs:575](../../crates/secode-provider/src/request.rs#L575). A public catalog entry alone is not executable support. |
| V06 Live connections, names, status, disconnect | Login/config [C-commands] | Auth commands [A-cli] | `/connect`, auth [O-cli] | model/setup [H-config] | **Present**: in-TUI key/OAuth connect, multiple named connections, rename/delete, renewal; CLI login/logout/auth status landed `076cf2bd7` [cli/command/help.rs:58](../../crates/secode-cli/src/command/help.rs#L58), `crates/secode-tui/src/connections.rs:1`. |
| V07 Explicit model fallback chain | — | fallback-model [A-cli] | Agent model options [O-agents] | Fallback configuration [H-config] | **Partial / PAR-25**: retries/account cooldown exist; no user-selected ordered *model* fallback contract [provider/opener.rs:1](../../crates/secode-provider/src/opener.rs#L1), [cli/command.rs:125](../../crates/secode-cli/src/command.rs#L125). Do not conflate account rotation and model switching. |
| V08 Fast/priority service tier | service_tier [C-config] | Fast mode [A-fast] | Provider options [O-models] | Provider options [H-config] | **Gap PAR-26**: reasoning effort exists, but no priority-tier request field/control [provider/request.rs:575](../../crates/secode-provider/src/request.rs#L575), [tui-r/slash.rs:50](../../crates/secode-tui/src/slash.rs#L50). Effort is not service tier. |

### Config and CLI

| ID / capability | Codex | Claude Code | OpenCode | Hermes reference | SE Code at baseline / gap |
| --- | --- | --- | --- | --- | --- |
| C01 Layered config, trust, instructions | config.toml/AGENTS [C-config] | Settings/CLAUDE/rules [A-settings] [A-memory] | JSONC/AGENTS [O-config] | YAML/context files [H-config] [H-context] | **Present**: user/profile/project precedence, JSONC, scoped instruction loading and Claude fallback [dogfood/instructions.rs:50](../../crates/secode-product/src/dogfood/instructions.rs#L50), `crates/secode-config/src/layer.rs:1`. File format differences are not missing capability. |
| C02 Session model/effort/permissions | Config/commands [C-config] | Flags/commands [A-cli] | Agent/model config [O-agents] | model/config [H-config] | **Present** in TUI/API; **CLI selection gap PAR-01** [cli/compatibility.rs:93](../../crates/secode-cli/src/compatibility.rs#L93). |
| C03 One-run config/prompt/tool overlays | `-c`, profile overrides [C-commands] | settings/system/tool flags [A-cli] | Custom config path [O-config] | CLI precedence [H-config] | **Partial / PAR-14**: profiles and agent definitions supply these settings; environment config overlays already exist; no explicit CLI flags for prompt append/tool narrowing/selected setting sources [cli/command.rs:125](../../crates/secode-cli/src/command.rs#L125), [cli/command/terminal.rs:23](../../crates/secode-cli/src/command/terminal.rs#L23). |
| C04 Safe/minimal startup | Feature/config switches [C-config] | bare/safe-mode [A-cli] | pure mode [O-cli] | Toolsets/profile config [H-config] | **Gap PAR-35**: no diagnostic startup mode to disable optional integrations while retaining mandatory enforcement [cli/command/terminal.rs:23](../../crates/secode-cli/src/command/terminal.rs#L23). Never implement this as a permission bypass. |
| C05 Migration of personal setup | — | Import/config [A-commands] | Configurable directories [O-config] | Migration [H-cli] | **Present**: Claude/Codex MCP, instructions, skills, agents, hooks import with preview, landed `9a142aefe` [cli/migration.rs:1](../../crates/secode-cli/src/migration.rs#L1). Unsupported imported metadata is not promised to execute. |
| K01 Headless text/JSON/JSONL, stdin | exec [C-exec] | print/input/output formats [A-cli] | run/json [O-cli] | run/json [H-cli] | **Present** for bounded prompt stdin and durable output envelopes [cli/command/help.rs:143](../../crates/secode-cli/src/command/help.rs#L143). **Bidirectional streaming-input gap PAR-34**; `--resident` keeps output/mail alive but does not make stdin a structured control stream. |
| K02 Resume/fork/background/worktree flags | CLI flags [C-commands] | CLI flags [A-cli] | CLI flags [O-cli] | CLI flags [H-cli] | **Present** with explicit combination restrictions [cli/command/help.rs:159](../../crates/secode-cli/src/command/help.rs#L159). |
| K03 Structured final output | output-schema [C-exec] | json-schema [A-cli] | — | JSON envelope [H-cli] | **Gap PAR-03**: envelope JSON is not final-answer schema validation [cli/command.rs:125](../../crates/secode-cli/src/command.rs#L125). |
| K04 Per-run work/cost limits | — | max-turns/budget [A-cli] | Agent steps [O-agents] | Runtime limits [H-config] | **Partial / PAR-04**: fixed/runtime bounds exist; no user-selected invocation caps [cli/command.rs:125](../../crates/secode-cli/src/command.rs#L125). |
| K05 Nonimage file prompt attachments | File context [C-commands] | File/piped context [A-cli] | run --file [O-cli] | file inputs [H-cli] | **Partial / PAR-12**: CLI images and TUI file mentions exist; `--file` explicitly refuses [cli/compatibility.rs:95](../../crates/secode-cli/src/compatibility.rs#L95). ACP embedded files are a separate supported entry. |
| K06 Shell completion, catalog/status commands | completion [C-custom] | doctor/agents/config [A-cli] | completion/models/stats/session [O-cli] | doctor/config/sessions [H-cli] | **Partial / PAR-13a–b**: status/auth/session rename/MCP operations exist; no shell completion or general session/model/usage inventory CLI [cli/command.rs:94](../../crates/secode-cli/src/command.rs#L94). TUI/API equivalents should be reused. |
| K07 Noninteractive dedicated review | exec/review [C-commands] | review workflows [A-cli] | agent run [O-cli] | agent run [H-cli] | **Partial / PAR-36**: native `/review` template exists; CLI has no named review command and refuses model/agent flags [dogfood/catalog_commands.rs:171](../../crates/secode-product/src/dogfood/catalog_commands.rs#L171). Generic run can still ask for review. |
| K08 Update/install/version | Installer/update [C-cli] | update/install [A-cli] | upgrade/uninstall [O-cli] | update [H-cli] | **Present**: verified local package update/rollback is the established product path; different download channels are not a capability gap [cli/compatibility.rs:60](../../crates/secode-cli/src/compatibility.rs#L60), `crates/secode-release/src/lib.rs:1`. No live update was attempted. |

### Performance and editor/remote surfaces

| ID / capability | Codex | Claude Code | OpenCode | Hermes reference | SE Code at baseline / gap |
| --- | --- | --- | --- | --- | --- |
| F01 Prompt caching / usage visibility | Caching config [C-config] | Automatic caching/costs [A-cache] | Model/compaction options [O-config] | Prefix/compression config [H-config] | **Present**, measured release criterion 2 accepted at 97.7%; live per-call/session cache and cost accounting [tui-r/usage.rs:1](../../crates/secode-tui/src/usage.rs#L1), `crates/secode-product/src/cache_accounting.rs:1`. Matched-task cache comparison remains criterion 3, not a missing-feature ticket. |
| F02 Auto/manual compaction, threshold control | Compaction settings [C-config] | compact/autocompact [A-commands] | Compaction [O-config] | Compression [H-config] | **Present**: provider-first OpenAI compaction, fallback summary and configured thresholds [dogfood/compaction_config.rs:1](../../crates/secode-product/src/dogfood/compaction_config.rs#L1). `keep.tokens` is explicitly ignored: **gap PAR-39** if recent-context retention control is required. |
| F03 Efficient rendering and idle work | Terminal UX [C-custom] | Terminal modes [A-interactive] | TUI [O-tui] | CLI [H-cli] | **Present mechanisms**, not a universal benchmark victory: incremental Markdown, bounded history, dirty-frame output [tui-r/present.rs:1](../../crates/secode-tui/src/present.rs#L1), `crates/secode-tui/src/markdown/incremental.rs:1`. Release criterion 11 records Windows echo/idle acceptance. Other competitor ceilings remain deferred. |
| I01 ACP editor protocol | IDE/app-server integrations [C-ide] | Native IDE integrations [A-ide] | `opencode acp` [O-acp] | `hermes acp` [H-acp] | **Present**: ACP session lifecycle, embedded context/images, permissions/config and cancellation [cli/acp.rs:1](../../crates/secode-cli/src/acp.rs#L1). SSE capability advertises false [cli/acp.rs:160](../../crates/secode-cli/src/acp.rs#L160); protocol-version/host interoperability beyond existing coverage is **unverified**, not “ACP missing.” |
| I02 Editor-native context and diffs | IDE extension [C-ide] | VS Code/JetBrains [A-ide] [A-jetbrains] | VS Code context shortcuts [O-ide] | ACP hosts [H-acp] | **Partial / PAR-27**: ACP exists; no Rust-product-owned IDE extension for active selection, current file, launch and inline review. Source inventory contains no extension package [cli/acp.rs:1](../../crates/secode-cli/src/acp.rs#L1). Frozen TS artifacts are not evidence of an active packaged Rust integration. |
| I03 Browser/mobile/cloud/remote continuation | Cloud delegation [C-cloud] | Remote Control/cloud [A-remote] | Web/server [O-web] [O-server] | Messaging gateway [H-readme] | **Partial / PAR-31**: authenticated remote service/attach and retained API exist [cli/command.rs:41](../../crates/secode-cli/src/command.rs#L41); no supported native-product browser/mobile control client or hosted job handoff. Desktop/ACP remain post-0.1 per release checklist. |
| I04 Scheduled autonomous tasks | Cloud automations [C-cloud] | loop/scheduled tasks [A-schedule] | External automation [O-cli] | cron [H-cron] | **Gap PAR-30**: resident mail wake is event delivery, not a user-facing bounded task scheduler [cli/command/help.rs:149](../../crates/secode-cli/src/command/help.rs#L149), [tools/catalog.rs:4](../../crates/secode-product/src/dogfood/tools/catalog.rs#L4). |

## Ranked tickets ready for assignment

Sizes are engineering estimates: S ≤ one day, M about 2–3 days, L about one week.
Each ticket is READY-TO-ASSIGN within its stated scope; larger follow-ups are named
separately. Qwen suitability assumes a precise spec sheet, bounded file envelope,
an ordinary reviewer and the existing affected gate. “No” means assign to a
frontier-model owner, not that implementation is impossible. No ticket authorizes
credential, permission or durable-state changes outside project law.

### PAR-01 — Select the model and agent from a headless invocation

Impact: high; repeatable scripts and local-model experiments currently require
configuration edits. Size M. Qwen: **yes**, after the selection/precedence contract
is fixed. Evidence: `crates/secode-cli/src/compatibility.rs:93` explicitly refuses
`--agent/--model`; the interactive entry also has no such options
(`crates/secode-cli/src/command/terminal.rs:23`).

Files: `crates/secode-cli/src/command.rs`, `command/terminal.rs`, `compatibility.rs`,
`run.rs`, and the existing session agent/model API client methods; focused CLI
public-seam tests. Reuse existing typed selection endpoints. Scope: `--model/-m`
and `--agent` for a new session, explicit refusal for incompatible resume/fork
combinations until separately specified; no credential or config rewrite.
Acceptance: a new headless session uses the requested offered agent/model;
unknown selections fail before a provider request; an existing session keeps its
selection unless an explicit, reviewed selection operation was requested.

### PAR-02 — Open the composer in an external editor

Impact: high for long prompts. Size S. Qwen: **yes**. Evidence:
`crates/secode-tui/src/slash.rs:50` and `actions.rs:11` enumerate the native
commands/actions without an editor action. Competitors expose this interaction.

Files: `crates/secode-tui/src/actions.rs`, `slash.rs`, `keys.rs`, a proposed
`external_editor.rs`, and terminal suspend/restore ownership in `console_control.rs`.
Scope: `/editor` plus a configurable action; resolve `VISUAL`, then `EDITOR`, with
bounded temporary UTF-8 input and explicit process failure feedback.
Acceptance: cancel and failed editor startup preserve the exact draft; successful
edit replaces it once without submitting; terminal mode and queued input recover
after both success and editor failure. Run the existing terminal interaction check.

### PAR-03 — Request validated structured final output

Impact: high for CI and tools consuming the agent's answer. Size M. Qwen: **no**
for the provider/runtime change; **yes** for the CLI adapter after its seam exists.
Evidence: `crates/secode-cli/src/command.rs:125` exposes output envelopes but no
final-answer schema; MCP tool output schema validation is a separate facility
(`crates/secode-product/src/dogfood/tools/mcp_call.rs:256`).

Files: CLI `command.rs`, `run.rs`, `output.rs`; provider request/serialization
owners in `crates/secode-provider/src/request.rs`; product request planning and
final-answer validation. Scope: `--output-schema <file>` with a documented bounded
JSON Schema subset, supported provider routing and an explicit unsupported-family
error. Acceptance: a conforming answer is emitted as validated JSON; malformed
schema is rejected before network work; nonconforming/provider-refused output has
a named failure and never masquerades as validated success.

### PAR-04 — Bound one headless run by user-selected work and cost limits

Impact: high for unattended work. Size M for the first slice (logical-step limit);
M follow-up for estimated-dollar limit. Qwen: **yes** for exposing an existing
step bound, **no** for cost enforcement. Evidence: CLI run options
(`crates/secode-cli/src/command.rs:125`) lack per-invocation caps; runtime already
reports `StepBudgetExhausted` (`crates/secode-product/src/dogfood/execution/observations.rs:296`).

Files: CLI `command.rs`, `run.rs`, existing runtime coordinator budgets, product
execution planning, `usage_accounting.rs`. Scope: first expose an integer
`--max-steps`; then a separately assignable `--max-budget-usd` using the existing
price/accounting owner and documented estimate semantics. A Step remains one
logical provider call, including the project's retry distinction.
Acceptance: a deterministic multi-step task stops before admitting step N+1,
retains completed output, and reports budget exhaustion; the cost follow-up
rejects missing prices as unknown and never treats unknown usage as zero.

### PAR-05 — Add an opt-in LSP diagnostics and navigation integration

Impact: high for code correctness and navigation. Size L, first slice limited to
one configured server and diagnostics/definition/references. Qwen: **no**.
Evidence: the built-in catalog (`crates/secode-product/src/dogfood/tools/catalog.rs:4`)
has text search and file tools but no LSP path. The source inventory contains no
language-server owner. Configured MCP remains a possible user workaround.

Files: proposed `crates/secode-product/src/dogfood/lsp/` for bounded process and
document lifecycle; tool adapters alongside `dogfood/tools/coding.rs`; existing
plugin/MCP process-supervision patterns and config source trust rules.
Acceptance: an explicitly configured language server returns diagnostics after an
agent edit and resolves a symbol definition/reference; stale versions are
discarded; server crash/cancellation is bounded and visible; an untrusted project
cannot start an executable merely by naming an extension. Process permission is
Tier 1 for that boundary. Do not add automatic server installation in this slice.

### Remaining tickets, in descending user-impact groups

The first implementation queue should be **PAR-11a, PAR-01, PAR-04 (step cap),
PAR-03, PAR-05, PAR-02, PAR-12, PAR-37**, in that order. Recoverability and reliable
automation rank above ecosystem breadth. These are parity priorities; admission is still
decided against the release scope and existing owners. Ticket IDs stay
stable across the batches rather than being renumbered when priority changes.

Every row below is a separate assignable slice, including letter-suffixed rows.
File paths starting `product/`, `cli/`, `tui/`, `provider/` or `session/` mean
`crates/secode-<name>/src/`; `tui/` means `secode-tui`. “New” marks proposed files.
Acceptance describes an observable check, not a request to build another harness.

| Ticket / user outcome | Size / Qwen with spec | Files / bounded scope | Acceptance test |
| --- | --- | --- | --- |
| **PAR-11a** — Undo ordinary edit/write effects as well as patches (S07) | M / **no**, restore integrity | `product/dogfood/tools/executor.rs`, `effects.rs`, `coding.rs`, `coding/checkpoints/`; extend the existing checkpoint owner, not another journal. Tier 1 for restore/durable records. | One prompt edits, creates and overwrites via different file tools; preview names all captured files and approved undo restores them. A changed preimage refuses without discarding another writer's changes. |
| **PAR-12** — Attach bounded text files in CLI prompts (K05) | S / **yes** | `cli/command.rs`, `compatibility.rs`, `run.rs`; reuse existing prompt file DTOs and content ownership. Initial slice: repeatable local UTF-8 `--file`, not URLs/PDF decoding. | Two files reach the new session as declared context; oversized/unreadable input fails before admission; existing image behavior remains. |
| **PAR-37** — Enter and leave plan mode (U13) | M / **no** for permission semantics; UI adapter **yes** | `tui/slash.rs`, `backend/commands.rs`, existing agent/model and permission-preset clients; product planning mode only through existing authority owner. | `/plan` visibly enters a planning session/mode that can inspect but cannot edit; approval to implement leaves that mode explicitly. Resuming preserves the selected state. |
| **PAR-08** — Run a human's `!command` directly (U06) | M / **yes** after permission contract | `tui/keys.rs`, `backend/commands.rs`, proposed `backend/shell.rs`; use the ordinary shell/permission executor and committed output. | `!git status` executes once in the attached workspace and its result becomes context. Cancel/deny launches nothing; leading `!` pasted as literal text is handled by the specified escape rule. |
| **PAR-16a** — Emit useful failure/request lifecycle events (H02) | M / **no**, event ordering | `session/notice/lifecycle_hook.rs`, `product/dogfood/hooks/`, `execution/`; first slice `PostToolUseFailure`, `PreModelRequest`, `PostModelRequest`, `Interrupt`. Additional config/worktree/model-switch events are a separate M slice using the same enum and owners. | Tool failure and transport retry emit their correctly scoped event once per declared boundary, with no success event fabricated and no extra provider request. |
| **PAR-16b** — Opt in to hook prompt/result payloads (H02) | M / **no**, data disclosure boundary | `product/dogfood/hooks/mod.rs`, `command.rs`, `plan.rs`, `tools/permission_hooks.rs`; version the payload contract and allow user-owned opt-in only. | A result-aware audit hook receives bounded tool result/status and a prompt hook receives the admitted prompt; credentials/private session data are absent; old metadata-only bindings retain their old payload. |
| **PAR-19** — Execute skill-local agent/tool scope (P02) | M / **no**; frontmatter parser alone **yes** | `product/dogfood/catalogs.rs`, `catalog_commands.rs`, existing `agent_definitions/`, `subagent/`; `tui/slash/expand.rs`. First slice `context: fork` plus named `agent` and narrower tool selection. | A fork-scoped skill runs once in a child with the named offered agent and tool ceiling; returning cannot widen the parent's permissions. Skill-local hooks are a follow-up using PAR-16/17, not arbitrary executable frontmatter. |
| **PAR-13a** — Read sessions/models/usage from scripts (K06) | S per command family / **yes** | `cli/command.rs`, `command/help.rs`, proposed `command/catalog.rs`; reuse versioned API clients and existing usage accounting. Initial slice list sessions and models, text/JSON. | CLI lists the same authorized rows as the TUI/API, paginates with a documented bound and reports unavailable service separately from an empty list. Stats/export follow as an S slice through existing APIs. |
| **PAR-13b** — Shell completion (K06) | S / **yes** | `cli/command.rs`, `command/help.rs`, new static completion assets or generator shared with command metadata. | Bash/Zsh/Fish/PowerShell completion proposes supported commands/flags and never executes a provider or reads credentials. |
| **PAR-36** — Headless review command (K07) | S / **yes**, after PAR-01 | `cli/command.rs`, `run.rs`, existing review template `product/assets/commands/review.txt`. Initial targets uncommitted changes/base/commit; PR resolution later. | `secode review --base <ref>` selects the existing reviewer definition and reports findings without edits; bad targets fail before provider work. |
| **PAR-14** — Explicit invocation prompt/tool overrides (C03) | M / **yes** for CLI plumbing with fixed authority spec | `cli/command.rs`, `command/terminal.rs`; existing `product/dogfood/catalog_config.rs` and agent/session configuration owners. Environment `SECODE_CONFIG`/`SECODE_CONFIG_CONTENT` overlays already exist. Scope: flags for prompt append and tool narrowing, with exact precedence; no new config layer architecture. | A one-run prompt append and tool exclusion affect only the chosen run, leave profile files unchanged and cannot lift a configured deny. |
| **PAR-35** — Diagnostic startup without optional integrations (C04) | M / **no**, mandatory enforcement must survive | `cli/command/terminal.rs`, product catalog/plugin/MCP startup. Scope: `--safe-mode` for optional customization plus visible explanation of suppressed sources; no “skip all config” shortcut. | A broken optional plugin cannot prevent this mode starting; mandatory user enforcement and credentials origin checks still operate. |
| **PAR-15a** — Inspect plugin status from TUI/CLI (P04) | S / **yes** | `cli/compatibility.rs`, `command.rs`, `tui/slash.rs`, existing `/api/plugin` list response in `product/dogfood/plugins.rs`. Scope read-only listing, provenance and named failure. | Lists active/blocked plugins and their origin/generation without treating project declarations as consent. |
| **PAR-15b** — Install approved plugin bundles (P04) | L / **no** | `crates/secode-plugin/src/declaration.rs`, `source.rs`; `product/dogfood/plugins/materialize.rs`; new CLI install/enable/remove adapter and user-owned installation store. First slice one pinned local bundle format; registry/marketplace fetching separately. | Inspect a pinned bundle before enabling; install exposes its declared skills/MCP/commands through existing owners; disable removes future exposure; project metadata cannot execute an install or forge user consent. Tier 1 for installation/authority. |
| **PAR-17a** — MCP tool hook handler (H03) | M / **no** | `product/dogfood/hooks/config.rs`, `inline.rs`, hook dispatch and existing MCP call leases. | A configured hook calls an already approved published MCP route; disconnect or scope change refuses; hook execution cannot recursively reinvoke itself without a finite bound. |
| **PAR-17b** — HTTP hook handler (H03) | M / **no** | Same hook owners; existing `crates/secode-web` transport policy/egress seam. No private parallel HTTP client. | Explicit user-owned URL receives the bounded event; timeout/cancellation is named; request/response limits and approved destination rules apply. |
| **PAR-17c** — Prompt/agent hook handlers (H03) | M / **no** | Existing hook decisions plus product provider/subagent planning. Start with one supported decision event; no general agent DSL. | A configured evaluator returns a validated allow/deny decision within a finite budget; invalid output does not grant authority and recursive hook evaluation cannot run indefinitely. |
| **PAR-18a** — Async observational hooks (H04) | M / **no** | `product/dogfood/hooks/command.rs`, `mod.rs`, service-owned cancellation/drain. | An opt-in observer runs without blocking the next model step; late output is attributed to the original event; shutdown cancels/drains it; it cannot return a retroactive permission decision. |
| **PAR-18b** — Stop hook can request bounded continuation (H04) | M / **no** | `product/dogfood/hooks/decision.rs`, execution coordinator and continuation owner. Scope one explicit continuation decision with finite repeat budget and visible cause. | A hook requesting one correction resumes exactly once; repeated stop requests exhaust a named cap; already completed effects are never replayed. |
| **PAR-06** — Cell-aware notebook tool (T11) | M / **yes** with exact schema and existing write adapter | New `product/dogfood/tools/coding/notebook.rs`, catalog and existing governed file effect. Scope cell-ID replace/insert/delete, preserve unrelated notebook metadata and outputs. | Change one named cell without rewriting another cell's content/metadata; invalid/missing IDs refuse; stale preimage changes fail safely. |
| **PAR-07** — Opt-in formatter after edits (T15) | M / **yes** after execution/authority spec | New `product/dogfood/tools/formatting.rs`, existing coding effect and configured command execution; user-owned formatter registry only. | Configured formatter runs once after a successful relevant edit, feedback includes any error and resulting diff, and a repository config cannot silently install or launch a formatter. |
| **PAR-11b** — Restore an earlier file checkpoint and redo (S08) | L / **no**, depends on PAR-11a | Existing `product/dogfood/undo_route.rs`, checkpoint selection/restore journal, `tui/backend/commands.rs`. First slice choose an existing captured prompt checkpoint; redo is a separate M follow-up with an explicit rescue checkpoint. | Preview names the selected point and consequences; approved restore rejects later external edits; redo restores the recorded postimage and new edits invalidate redo visibly. Tier 1. |
| **PAR-20** — Opt-in persistent automatic memory (P05) | L / **no** | New `product/dogfood/memory/`; existing instruction projection, artifact/storage and user-owned config paths. First slice bounded per-project memory file with preview/approve/disable controls; automatic extraction separately. | Approved memory survives a new session in the same project; another project cannot read it without an explicit scope grant; disabling prevents injection; unknown/unreadable is distinct from empty. Tier 1 for durable lifecycle/permissions. |
| **PAR-21** — Search prior authorized sessions (S10) | M / **no** for authorization/query owner; read-only adapter **yes** | Existing session/history query owner and new `product/dogfood/tools/session_search.rs`; no unrestricted DB tool. | Search returns bounded excerpts from authorized sessions with source/session IDs; unrelated principal/project data is excluded; results paginate and unavailable index is named. Tier 1 for new cross-session access. |
| **PAR-22** — Native Gemini provider (V04) | L / **no** | `provider/request.rs`, native serialization/stream normalization, platform routing and existing connections/catalog. First slice text/tool loop/images with API key; do not make catalog metadata imply runtime support. | A normal product session streams text and one tool round trip through the native protocol, reports provider usage faithfully and names unsupported modality/accounting. No paid acceptance call without existing authorization. |
| **PAR-23** — AWS Bedrock connection (V05) | L / **no** | Provider protocol/transport adapters plus `crates/secode-platform/src/` provider inputs and `secode-connections`; reuse existing origin-bound credentials. Initial slice one documented Bedrock protocol/credential chain, not all AWS model APIs. | Configured Bedrock model completes text/tool round trip with correctly signed requests, cancellation and usage; wrong region/credential origin refuses before disclosure. Tier 1 for credentials. |
| **PAR-24** — Google Vertex connection (V05) | L / **no** | Same provider/platform/connection owners as PAR-23, Vertex-specific native adapter. First slice one native API and explicitly bound project/location/token source. | Request goes only to the configured project/location with its bound credential; renewal/cancellation work; tool/usage normalization matches the product contract. Tier 1. |
| **PAR-25** — Explicit ordered model fallback (V07) | M / **no** | `provider/opener.rs`, product routing/selection, user config and usage display. First slice user-specified overload/unavailable fallback; no silent quality downgrade. | Primary overload selects the configured next offered model once, UI/history show the switch and actual usage, and permission/tool ceilings survive; ordinary validation errors do not trigger a switch. |
| **PAR-26** — Select provider priority tier (V08) | M / **yes** for a single protocol with a fixed schema | `provider/request.rs` and request serialization, supported route metadata, `tui/models.rs` or settings adapter. First slice OpenAI `service_tier`; Anthropic fast mode separately using its published contract. | Supported selection reaches the request and is shown as requested/observed rather than guaranteed latency; unsupported family/model gives a named refusal; default requests preserve their existing shape. |
| **PAR-39** — Honor recent-context retention control (F02/V2-01) | M / **no**, durable replay | `product/dogfood/compaction_config.rs`, `continuation/compaction.rs`, `continuation/retained.rs`, existing compaction event owner. | `keep.tokens` changes the retained recent tail after compaction/resume within hard input bounds; old sessions replay under their recorded setting; a provider-native route reports when this control cannot be honored. Tier 1 for the new durable fact. |
| **PAR-41** — Opt-in cache warming during idle (V2-02) | M / **no**, provider/accounting lifecycle | Existing product session activity/coordinator and provider planner; new bounded warming adapter, user-owned config only. | A fake clock triggers at most the configured idle requests/window; real input cancels/resets the schedule; no tool executes or durable conversation input appears; warming usage/cost is counted distinctly. Default stays off. |
| **PAR-42** — Background summary preparation (V2-03) | L / **no**, context identity races | Existing compaction/retained-context owner; in-memory prepared summary tied to exact instruction/model/history identity. | Fake provider prepares while work continues; a valid summary swaps in once; changed model/instructions/history discards it and uses normal compaction. Unused work remains accounted for. |
| **PAR-27** — IDE context bridge (I02) | M / **yes** for a thin VS Code adapter | New editor extension outside core crates, launch/selection/file-range bridge over the existing ACP/API. First slice launch/focus terminal and attach current selection; inline apply UI separately. | From VS Code, launch/focus the installed product and send the selected file range without copying unrelated files; disconnect preserves session state; no second engine or credential store. |
| **PAR-43** — Multiple native session tabs (V2-04) | M / **yes** with draft/attachment ownership spec | `tui/ui.rs`, sessions and backend attach owner; new tab model bounded by a fixed cap. | Switching tabs preserves each session's draft/images/scroll position; a late result updates its originating tab; close/reopen cannot submit the wrong draft or duplicate a session. |
| **PAR-10** — User-provided color themes (U04) | S / **yes** | `tui/palette.rs`, settings/preferences and highlight theme selection. First slice a bounded color-token file and picker; no arbitrary code or mandatory new theme engine. | Valid theme previews/saves; malformed or low-capability terminal input leaves the current readable palette; restarting restores the selected theme. |
| **PAR-09** — Vim input mode (U07) | M / **yes**, keep grammar bounded | `tui/editor.rs`, `keys.rs`, display status. First slice insert/normal, motions, delete/change/yank, text undo; no reimplementation of all Vim. | Mode is visible; Esc interrupts running work according to the agreed precedence and otherwise enters normal mode; Unicode motions and undo preserve draft bytes. |
| **PAR-13c** — Import exported native session JSON (S11) | L / **no**, durable import | Existing migration/session import ownership, new CLI adapter and validation of the product's own exported format. Foreign vendor formats are separate tickets after schema research. | Export/import into an empty disposable profile preserves conversation provenance without reexecuting effects; duplicate/corrupt IDs refuse atomically. Tier 1. |
| **PAR-34** — Bidirectional headless structured stream (K01) | M / **no**, input/ack ownership | `cli/run/resident.rs`, `run/jsonl.rs`, a new bounded input decoder; existing inbox submission/cancel seams. | Two structured prompts and an interrupt on stdin receive stable acknowledgments and scoped output; disconnect/retry cannot duplicate admitted input; malformed records fail explicitly. |
| **PAR-40** — Authorized MCP sampling (M08) | M / **no**, model-use authority | `product/dogfood/mcp/channel.rs`, a new sampling adapter through existing provider planning and human approval. | Approved `sampling/createMessage` gets one bounded model result under the caller's explicit model/cost constraints; unapproved sampling makes no provider call; cancellation and usage are retained. |
| **PAR-28** — Bundled browser connection workflow (T13) | M / **yes** for a pinned MCP integration after security spec | Existing MCP/plugin connection and permission UI; package an approved browser adapter and skill. No second browser runtime in core. | User connects the adapter, navigates a local test app, inspects a screenshot and performs one approved action; disconnect closes only owned resources; published tool origin is visible. |
| **PAR-29a** — Image-generation integration (T14) | M / **yes** for adapter/UX | Existing MCP/plugin and artifact/image owners. First slice one explicit provider integration/skill, not native support for every image service. | Generate and revise an image into a bounded artifact displayed in the transcript; source images retain provenance; provider refusal is named and costs are reported. |
| **PAR-29b** — Voice input/output (T14) | L / **no** for capture/playback lifecycle | New optional audio adapters, TUI action and existing prompt submission/tool integration. First slice push-to-talk transcription; speech playback a separate M slice. | Explicit recording becomes an editable draft once; cancel sends nothing; recording indicator and resource release survive errors; audio is not stored without the specified retention policy. |
| **PAR-30** — Scheduled prompts (I04) | L / **no**, durable scheduling | Existing session/inbox execution and injected clock plus new scheduler owner. First slice local one-shot and finite recurring jobs, list/pause/cancel; external platform delivery separately. | Restart recovers a due job exactly once; cancellation prevents later admission; missed intervals follow a specified finite catch-up policy; no unbounded queue or ambient-clock shortcut. Tier 1 for durable scheduling. |
| **PAR-31a** — Remote browser/mobile control (I03) | L / **no** for authentication; client-only view **yes** | Existing authenticated API/event stream plus new supported web client. First slice session read/steer/approval without public pairing or cloud hosting. | A second authenticated client can observe and steer the selected session; expired credentials fail closed; reconnect restores cursor/draft without duplicate admission. Tier 1 at auth boundary. |
| **PAR-31b** — Hosted job handoff (I03) | L / **no** | Existing remote service/session/worktree owners and a new explicit remote-run adapter. Start with a user-configured remote SE service, not an invented hosted fleet. | Submit a task to that service, reconnect to its session and retrieve a reviewable diff with provenance; failed transfer never silently runs locally. |
| **PAR-38** — Supported desktop client for the Rust product (V2-05) | L / **no**, packaging/update boundary | Retained renderer integration and existing product/server/release packaging; identify its current supported build before adding code. First slice package the existing API-backed renderer, not redesign the UI. | Fresh install starts the Rust engine and can connect, prompt, inspect a diff and resume after app restart. Existing update/rollback and credential boundaries hold. Post-0.1; not a release gate. |
| **PAR-32** — Shared team task dependencies (S09) | L / **no** | Existing subagent/session/mail owners; new minimal shared task assignment state only if the parked campaign owner cannot supply it. First slice bounded task IDs, assignee and prerequisites; no campaign-control redesign. | Two workers cannot both claim one ready task; a dependency blocks launch until committed completion; parent/child visibility and cancellation remain scoped. Tier 1 for new durable claims. |
| **PAR-33** — MCP facade for an existing SE session (M09) | M / **no** for access boundary; protocol adapter **yes** | New `cli` MCP server adapter over the existing authenticated session API. First slice inspect/history/submit; no tool that bypasses normal permission checks. | An MCP client can list authorized sessions and submit one idempotent prompt; foreign session IDs refuse; no side-channel transcript or credential access. |
| **PAR-44** — Minimal / screen-reader terminal presentation (V2-06) | M / **yes** for presentation with fixed accessibility spec | Existing TUI transcript/backend and CLI text renderer; small alternate presentation entry, sharing execution/session owners. | Flat text has stable labels and no decorative animation; prompt, interrupt, permission and question flows remain usable through an actual terminal/screen-reader check. No duplicate engine. |

### Dependency and admission notes

PAR-01, 02, 06, 10, 12, 13a, 13b and 36 are the best initial spec-sheet/Qwen
candidates. PAR-04's step-cap adapter, PAR-14 and PAR-26 also fit after their
contracts are explicit. “Qwen yes” never means self-review or bypassing the
affected gate. Security, durable replay, provider normalization and lifecycle
changes need a stronger owner and the standing Tier 1 treatment where named.

PAR-11b depends on PAR-11a; PAR-36 depends on PAR-01; plugin-distributed hook/skill
features depend on the relevant hook/skill adapters, not merely PAR-15b's installer.
PAR-31/38 and scheduling/team expansion remain beyond 0.1 unless admitted.
None of these estimates consumes another seat's current assignment.

## OpenCode V2 supplement

V1 and V2 coexist and share the `opencode` executable name. The main matrix's
immutable OpenCode citations describe V1. These V2 observations are separately
retrieved from the live official [V2 documentation][O2-intro], so V1 omissions are
not silently treated as the current product boundary.

| ID / feature | V2 competitor behavior | SE evidence and resulting ticket |
| --- | --- | --- |
| V2-01 Compaction retention | Configurable recent tail plus token/percentage buffer [O2-compact] | Tail setting is accepted/ignored, numeric buffer only [dogfood/compaction_config.rs:1](../../crates/secode-product/src/dogfood/compaction_config.rs#L1): PAR-39 includes percentage parsing as a separate S adapter after the retained-state contract. |
| V2-02 Idle cache warming | Opt-in finite idle warming window [O2-warm] | Ordinary provider calls only; no warming owner in product/provider inventory [provider/opener.rs:1](../../crates/secode-provider/src/opener.rs#L1): PAR-41. Socket keepalive is not prompt-cache warming. |
| V2-03 Background compaction | Experimental advance summary preparation [O2-compact] | Existing summary path runs when compaction is admitted [product/continuation/compaction.rs:1](../../crates/secode-product/src/dogfood/continuation/compaction.rs#L1): PAR-42. No measured speedup is assumed. |
| V2-04 Session tabs | Switch/close/reopen session tabs [O2-tui] | One selected session in native UI [tui-r/ui.rs:1](../../crates/secode-tui/src/ui.rs#L1): PAR-43. Session picker already exists. |
| V2-05 Desktop, paired web control | Desktop download and one-time pairing [O2-intro] | Retained renderer exists but release explicitly defers desktop; CLI pair refuses [cli/compatibility.rs:46](../../crates/secode-cli/src/compatibility.rs#L46): PAR-38/31. This research did not qualify a current desktop package. |
| V2-06 Minimal terminal | `opencode mini` [O2-cli] | One native terminal entry and text headless output [cli/command/terminal.rs:23](../../crates/secode-cli/src/command/terminal.rs#L23): PAR-44; also covers Claude's documented screen-reader presentation [A-cli]. |
| V2-07 Human controls | `!`, editor, undo/redo, side questions [O2-tui] | Side questions/forks exist; external editor, direct shell and redo remain PAR-02/08/11b. |
| V2-08 CLI/provider/editor contracts | Model/agent/file flags, native JSON import, ACP [O2-cli] [O2-acp] | Confirmed gaps PAR-01/12/13c; ACP already present [cli/acp.rs:1](../../crates/secode-cli/src/acp.rs#L1). |

## History checks and limits

Before opening gaps, the audit read landed `dev` history for the implicated CLI,
provider request, native slash/action, tool-catalog, hook, checkpoint and MCP
paths, then inspected their actual dispatch/schema code. Absence claims rest on
closed parsers/enums or missing ordinary dispatch, not backlog text or commit
subjects alone. A second canonical check reached `af339db48` on 2026-11-04; its intervening changes concern credential-protection batching, ephemeral composition and tests, not these closed catalogs or command parsers. The baseline includes the connection, sandbox, import, skill,
subagent, MCP and TUI landings named above.

This audit does not claim exhaustive behavioral equivalence: paid model calls,
real desktop/IDE operation, screen-reader use and target-platform containment were
not rerun. Interoperability questions marked unverified must be probed before
someone opens a missing-feature ticket. Vendor billing/admin/social commands and
experimental research previews are not interchangeable with core CLI flags;
their relevant product outcomes map to the provider, integration, automation and
remote-surface tickets rather than requiring a copy of every vendor command name.

## Additional public command outcomes

This table records distinct advertised outcomes that would be lost by grouping
all slash commands under “commands supported.” It does not require SE to adopt
vendor spelling or account-specific billing commands. `product/`, `cli/` and
`tui/` paths follow the ticket shorthand above.

| Outcome / competitor reference | SE baseline evidence / gap | READY-TO-ASSIGN ticket: size, Qwen, files, acceptance |
| --- | --- | --- |
| Archive/unarchive/delete sessions [C-commands] [O2-cli] | Native picker is selection/search, not lifecycle management: `tui/sessions.rs:1`; CLI enum `cli/command.rs:94`. ACP deletion exists: `cli/acp.rs:88`. | **PAR-45**, M, Qwen **yes** for archive/list UI, **no** for deletion policy. Add picker/CLI adapters over existing session APIs; archive preserves resume-by-ID and unarchive restores visibility; delete requires explicit preview and cannot bypass durable deletion rules. No new deletion backend in this slice. |
| Persistent task goal and continuation [C-commands] [A-commands] | Planning/checklist exists; neither native slash catalog nor built-in tool catalog has a goal lifecycle: `tui/slash.rs:50`, `product/dogfood/tools/catalog.rs:4`. | **PAR-46**, M, Qwen **no**. Existing session execution/coordinator plus proposed goal state and UI adapter. An explicit goal survives restart, continues only within finite budgets, can be cleared, and reports achieved/exhausted/blocked distinctly; no implicit goal creation from an ordinary request. Tier 1 for durable state. |
| Inspect/stop background terminals as the human [C-commands] [A-commands] | Model handles/polling exist (`product/dogfood/tools/terminal.rs:50`); no native process-management action (`tui/actions.rs:11`, `slash.rs:50`). | **PAR-47**, M, Qwen **yes** for UI after scoped listing/stop seam. Add `tui` terminal pane and a product API over the existing session terminal registry. List only this session's terminals; stop by exact owned operation, never PID pattern; a late process exit cannot stop a reused process. |
| Hook inspection, enablement diagnostics [C-commands] [A-hooks] | Documented command hooks but no `/hooks` action (`tui/slash.rs:50`); existing resolution `product/dogfood/hooks/config.rs:1`. | **PAR-48**, S for read-only inspector, Qwen **yes**. Add native/CLI inspection over the resolved hook catalog showing origin/event/matcher/status, without secrets. Acceptance: mandatory and withheld hooks are explained, unreadable config is named, inspection runs no hook. Editing is a separate M boundary. |
| Effective config layers and source explanation [C-commands] [O-config] [H-cli] | Source fold exists (`product/dogfood/catalog_config.rs:1`), but `/settings` is display preferences and `/context` is instruction capture. | **PAR-49**, S, Qwen **yes**. Add read-only `/config` or CLI inspector using existing config projection and redaction. Acceptance: one overridden value shows winning source and rejected/narrowing sources, with secrets excluded; no config writes/provider call. |
| Change workspace / add an authorized root interactively [C-commands] [A-commands] | CLI can start another repository; worktree commands manage checkouts, but no native `/cd` or `/add-dir` dispatch (`tui/backend/commands.rs:160`). | **PAR-50**, M, Qwen **no** for root authority; picker adapter **yes**. Reuse session placement and sandbox root approval, no ambient cwd mutation. Acceptance: opening a directory preserves the old session/draft, creates or selects the correct new placement; adding a root requires explicit scoped consent and cannot silently import its executable config. |
| Customize footer/title items [C-commands] [A-commands] | Fixed footer/usage and four display preferences (`tui/turn_footer.rs:1`, `settings.rs:16`). | **PAR-51**, S, Qwen **yes**. Add bounded selection/order of existing status items to native preferences. Acceptance: changing items preserves session/cost truth, narrow terminals remain usable and preferences survive restart. No arbitrary shell statusline in first slice. |
| Optional terminal pet / cosmetic companion [C-commands] | No such native action (`tui/actions.rs:11`). | **PAR-52**, S, Qwen **yes**, lowest impact. New optional native widget and display preference; off by default, no network or model use. Acceptance: enabling adds a bounded decorative item; disabled/hidden has no timer or idle wake. Admit only after meaningful parity work. |
| Automated risk review of approval requests [C-commands] [A-permissions] | Preset/rule classification exists (`product/dogfood/permission_preset.rs:1`); no model-based automatic risk-review stage in that path. | **PAR-53**, L, Qwen **no**. Existing permission owner plus a separately bounded advisory classifier; retain hard denies and explicit user approval escalation. Acceptance: classifier cannot override an explicit deny or missing authority, ambiguous requests remain attended, and decision/cost/failure is visible. Tier 1; not required merely to copy a vendor mode label. |
| Diagnose installation/startup from the CLI [C-commands] [H-cli] | `status` and auth status exist (`cli/command/help.rs:109`), no `doctor` command in the closed CLI enum. | **PAR-13d**, S, Qwen **yes**. `cli/command.rs`, new read-only `command/doctor.rs`, existing service/profile discovery. Acceptance: report binary/profile/service/MCP startup problems distinctly, redact credentials and perform no repair, installation or provider request by default. |
| Sanitized export [O2-cli] | Complete public-history export exists but has no selectable sanitize policy: `crates/secode-client/src/client/export.rs:1`, native `/export` dispatch `tui/backend/commands.rs:243`. | **PAR-54**, M, Qwen **yes** once the exact field policy is supplied. Add an explicit sanitized export view that omits prompt/tool/file bodies while preserving structure and source IDs. Acceptance: seeded private content and artifacts are absent, ordinary export remains complete; do not label heuristic secret masking as guaranteed anonymization. |
| Read PDF pages [A-tools] [O2-attachments] | Raster-only viewer, UTF-8 read, no PDF tool catalog entry (T16). | **PAR-55**, M, Qwen **yes** for a pinned renderer adapter with a fixed process/limit spec. Add bounded page-range rendering/text extraction through existing read and process authority; acceptance: selected pages produce readable artifacts/text, an invalid/encrypted/oversized PDF has a named result, and cancellation stops the owned renderer. |
| Read notebook cells and their outputs [A-tools] | Raw JSON reads exist, no structured notebook projection (T17). | **PAR-06b**, M, Qwen **yes**. Add read-only notebook projection beside PAR-06's cell editor, reusing image artifact handling; acceptance: code/Markdown/output cells remain associated with their cell IDs, image outputs reach the model with bounds, malformed notebook is not presented as empty. |
| Preserve BOM-tagged UTF-16LE edits [A-tools] | UTF-8-only file read/edit (T17). | **PAR-56**, M, Qwen **yes** with a bounded codec spec. Extend existing read/edit codec ownership, no global text abstraction; acceptance: a BOM-tagged Windows file round-trips the requested edit with its encoding and line endings, invalid byte sequences refuse unchanged, and ordinary UTF-8 behavior remains. |

### Flag coverage by semantic family

The [Codex command reference][C-commands], [Claude CLI reference][A-cli],
[OpenCode V1 CLI][O-cli], [V2 CLI][O2-cli] and [Hermes CLI][H-cli] are the flag
inventories. These mappings identify the affected behavior; aliases that reach
the same supported behavior do not create extra tickets.

| Flags / family | SE outcome or ticket |
| --- | --- |
| Help/version; repository/path; profile; local/remote/private server | Implemented by terminal command and `ConnectionChoice` (`cli/command/terminal.rs:23`, `cli/command.rs:41`); vendor flag spelling differs. |
| Session ID, continue/resume, fork, title, background, worktree, ephemeral | Implemented with documented combination restrictions (`cli/command/help.rs:159`). A new unsupported combination needs a concrete user journey, not an automatic alias ticket. |
| Model, agent, variant/effort | Effort implemented; model/agent flags PAR-01. |
| Print/run, prompt, stdin, image, file | Text/image implemented; generic file PAR-12; streaming input/control PAR-34. |
| JSON/stream JSON and output schema | Durable JSON/JSONL implemented; final-answer schema PAR-03. A log envelope is not a schema-constrained answer. |
| Sandbox, approval mode, auto/bypass, allowed/disallowed tool overrides, extra directories | Presets/auto/bypass implemented; invocation tool narrowing PAR-14; interactive extra roots PAR-50; automatic model risk review PAR-53. |
| Settings/config file/inline config, settings sources, system-prompt append/replace, agent definitions | Native profile and `SECODE_CONFIG`/`SECODE_CONFIG_CONTENT` already support config overlays; explicit prompt/tool flags PAR-14. Do not add a second config owner. |
| Max steps/turns, max budget, fallback model, fast tier | PAR-04, PAR-25, PAR-26; fixed internal limits are not per-invocation controls. |
| Bare/pure/safe/restricted startup | PAR-35 for optional-customization suppression; external sandbox restriction is X02. No promise to reproduce a vendor's unrestricted evaluation mode. |
| MCP configuration/auth, plugin directory/marketplace, debug/doctor/completion | MCP management present; plugin management PAR-15, inspection PAR-48/49, doctor PAR-13d, completion PAR-13b. |
| Cloud/remote-control/web/desktop/IDE/tmux/voice-specific options | Product outcomes PAR-27/29/31/38/44; process launch under an existing terminal multiplexer is possible today and does not itself require a native tmux flag. |

## Sources

OpenAI links are official OpenAI documentation; Claude links are public Anthropic documentation. OpenCode V1 and Hermes links are immutable source-document links. OpenCode V2 is separately identified below rather than silently substituting it for V1. SE links are local source at the pinned baseline.

[C-cli]: https://learn.chatgpt.com/docs/codex/cli
[C-commands]: https://learn.chatgpt.com/docs/developer-commands?surface=cli
[C-config]: https://learn.chatgpt.com/docs/config-file/config-reference
[C-prompt]: https://developers.openai.com/cookbook/examples/gpt-5/codex_prompting_guide
[C-web]: https://learn.chatgpt.com/docs/web-search
[C-image]: https://learn.chatgpt.com/docs/image-inputs
[C-plugins]: https://learn.chatgpt.com/docs/skills-and-plugins
[C-custom]: https://learn.chatgpt.com/docs/cli-customization
[C-agents]: https://learn.chatgpt.com/docs/agent-configuration/subagents
[C-exec]: https://learn.chatgpt.com/docs/non-interactive-mode
[C-cloud]: https://learn.chatgpt.com/docs/cloud
[C-security]: https://learn.chatgpt.com/docs/agent-approvals-security
[C-mcp]: https://learn.chatgpt.com/docs/extend/mcp
[C-hooks]: https://learn.chatgpt.com/docs/hooks
[C-ide]: https://learn.chatgpt.com/docs/codex/ide
[A-tools]: https://code.claude.com/docs/en/tools-reference
[A-interactive]: https://code.claude.com/docs/en/interactive-mode
[A-cli]: https://code.claude.com/docs/en/cli-reference
[A-commands]: https://code.claude.com/docs/en/commands
[A-sessions]: https://code.claude.com/docs/en/sessions
[A-checkpoint]: https://code.claude.com/docs/en/checkpointing
[A-memory]: https://code.claude.com/docs/en/memory
[A-agents]: https://code.claude.com/docs/en/sub-agents
[A-teams]: https://code.claude.com/docs/en/agent-teams
[A-permissions]: https://code.claude.com/docs/en/permissions
[A-sandbox]: https://code.claude.com/docs/en/sandboxing
[A-settings]: https://code.claude.com/docs/en/settings
[A-mcp]: https://code.claude.com/docs/en/mcp
[A-hooks]: https://code.claude.com/docs/en/hooks
[A-skills]: https://code.claude.com/docs/en/skills
[A-plugin]: https://code.claude.com/docs/en/plugins-reference
[A-chrome]: https://code.claude.com/docs/en/chrome
[A-ide]: https://code.claude.com/docs/en/vs-code
[A-jetbrains]: https://code.claude.com/docs/en/jetbrains
[A-remote]: https://code.claude.com/docs/en/remote-control
[A-schedule]: https://code.claude.com/docs/en/scheduled-tasks
[A-third-party]: https://code.claude.com/docs/en/third-party-integrations
[A-fast]: https://code.claude.com/docs/en/fast-mode
[A-cache]: https://code.claude.com/docs/en/costs
[O-tools]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/tools.mdx
[O-cli]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/cli.mdx
[O-tui]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/tui.mdx
[O-config]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/config.mdx
[O-providers]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/providers.mdx
[O-models]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/models.mdx
[O-agents]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/agents.mdx
[O-custom]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/custom-tools.mdx
[O-lsp]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/lsp.mdx
[O-format]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/formatters.mdx
[O-keys]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/keybinds.mdx
[O-themes]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/themes.mdx
[O-mcp]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/mcp-servers.mdx
[O-permissions]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/permissions.mdx
[O-plugins]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/plugins.mdx
[O-skills]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/skills.mdx
[O-server]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/server.mdx
[O-web]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/web.mdx
[O-acp]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/acp.mdx
[O-ide]: https://github.com/anomalyco/opencode/blob/b3fd4a75fe539a65168139cec2edd29ceb7724f6/packages/web/src/content/docs/ide.mdx
[H-tools]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/features/tools.md
[H-cli]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/reference/cli-commands.md
[H-context]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/features/context-files.md
[H-delegate]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/features/delegation.md
[H-config]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/configuration.md
[H-memory]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/features/memory.md
[H-mcp]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/features/mcp.md
[H-plugins]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/features/plugins.md
[H-skills]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/features/skills.md
[H-voice]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/features/voice-mode.md
[H-acp]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/features/acp.md
[H-cron]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/website/docs/user-guide/features/cron.md
[H-readme]: https://github.com/NousResearch/hermes-agent/blob/71736c9f32597bbca9d89cede5aa136e55946b2f/README.md
[O2-intro]: https://opencode.ai/v2/docs/
[O2-compact]: https://opencode.ai/v2/docs/compaction
[O2-warm]: https://opencode.ai/v2/docs/warming
[O2-tui]: https://opencode.ai/v2/docs/cli/tui
[O2-cli]: https://opencode.ai/v2/docs/cli/commands
[O2-acp]: https://opencode.ai/v2/docs/cli/acp
[O2-attachments]: https://opencode.ai/v2/docs/attachments
