# Remote MCP and sign-in

Connected servers can supply tool-usage instructions during initialization.
Grapt includes them in model context when the agent can access a server tool,
refreshes them after reconnection, and removes them when no longer applicable.
Server instructions do not grant permissions or override the user's instructions.

With the product running, manage servers from the CLI:

```sh
grapt mcp add docs --service /path/to/profile --url https://mcp.example.com/mcp
grapt mcp add local --service /path/to/profile --env MODE=read -- my-mcp-server --stdio
grapt mcp list --service /path/to/profile --json
grapt mcp remove docs --service /path/to/profile
```

Add and remove edit the user configuration selected by `GRAPT_CONFIG_DIR` (the
pre-rename `SECODE_CONFIG_DIR` is still accepted when `GRAPT_CONFIG_DIR` is unset), <!-- grapt-rename: keep -->
or `$XDG_CONFIG_HOME/grapt` (default `~/.config/grapt`). An install from before
the rename that has only `~/.config/secode` keeps using it, with a notice. <!-- grapt-rename: keep -->
Existing JSONC comments and unrelated settings are retained. If both
`grapt.json` and `grapt.jsonc` exist, consolidate them into one file before
editing with the CLI. A directory holding only the pre-rename
`secode.json` or `secode.jsonc` is read and edited in place under that name; <!-- grapt-rename: keep -->
when both spellings exist, the `grapt` files win and the others are ignored
with a notice. Nothing is renamed automatically. On Unix the directory
must not be writable by other users (`chmod go-w /path/to/config-directory`).
Declarations in other configuration layers remain in place; `mcp list` shows the
effective catalog for the selected repository. Remote additions enable OAuth for
HTTPS URLs. Local environment settings use repeated `--env KEY=VALUE` options;
keep secrets out of shell history.

You can also edit user configuration or the service profile's `grapt.json` /
`grapt.jsonc` directly:

```json
{
  "mcp": {
    "servers": {
      "docs": {
        "type": "remote",
        "url": "https://mcp.example.com/mcp",
        "oauth": true
      }
    }
  }
}
```

Project-owned MCP declarations cannot start processes or contact remote servers
until you approve the exact declaration. Alternatively, put it in user-owned
configuration.

Start the product with that profile, then sign in:

```sh
grapt mcp login docs --service /path/to/profile
grapt mcp logout docs --service /path/to/profile
```

Login opens a browser and prints the authorization URL. Use `--no-browser` to open the URL yourself. The command waits for the browser callback and confirms that the product saved the credential. Keep authorization URLs private: they contain sign-in state.

If the authorization server requires a pre-registered public client, add `--client-id YOUR_CLIENT_ID`. Optional `--scope "scope1 scope2"` selects scopes explicitly. The client must support S256 PKCE and the loopback callback URI; Grapt uses its own dynamically registered public client when the server supports registration. Add `--directory /path/to/repository` when selecting configuration for a particular location.

OAuth requires an HTTPS server URL and `oauth: true`. Opening a session or calling a tool never starts browser sign-in. Saved credentials refresh automatically when needed. Signing in again or signing out retires held sessions, so approvals from the previous connection cannot authorize the replacement.

In the R TUI, `/mcp enable <server>` and `/mcp disable <server>` save the
server's enabled state in user configuration. They preserve the declaration's
command, environment, and comments. A declaration must already exist in that user
configuration; project declarations require separate approval.

`/mcp reconnect <server>` retires only that server's current connection. After any
of these actions, the pane reads the catalog again and reports the actual
connection status. A failed change is reported without retrying it. Input and
interrupts remain available while the operation is pending.

The authenticated operator API accepts `POST /api/mcp/{server}/reconnect`, with the
same `directory` or `session` query selection as the catalog. It retires the named
connection and clears its startup backoff; the next catalog refresh starts it
again. Other servers keep their connections. Disabled or unavailable declarations
are refused, and reconnect never repeats a tool call.

For a static token, omit `oauth` and configure a header instead:

```json
{
  "mcp": {
    "servers": {
      "docs": {
        "type": "remote",
        "url": "https://mcp.example.com/mcp",
        "headers": { "Authorization": "Bearer YOUR_TOKEN" }
      }
    }
  }
}
```

Static Authorization headers and OAuth cannot be combined. Transport-owned headers, including Host, session ID, protocol version and event cursor, cannot be overridden.

The transport supports Streamable HTTP responses using JSON or SSE. It preserves the negotiated MCP session and protocol version. Interrupted SSE streams resume only from a committed event cursor; an uncertain POST outcome is reported instead of replaying a tool call. Redirects must preserve the method and configured origin. HTTP is accepted only for literal loopback addresses.

In the R TUI, run `/mcp login <server>` to display the browser sign-in URL.
After completing it in your browser, run the same command again to check the
result. `/mcp logout <server>` signs out. Both commands use the selected
project on the backend, including when the TUI runs on another machine.

Project declarations remain blocked until you approve them. Run
`/mcp approve <server>` to inspect the selected declaration, its source and working
directory. Run the same command again to approve that exact preview and enable
it. Approval permits the declaration to start a process or connect to its server;
changing the declaration invalidates the approval. A project cannot approve itself.
Use `/mcp disable <server>` to retain approval while stopping use of the server,
`/mcp enable <server>` to restore it, or `/mcp revoke <server>` to withdraw approval.
These decisions are stored in your user configuration for the selected directory.
An explicit approval or revocation there takes precedence over approval records
in other config sources; revocation remains effective after restart.

Grapt retains at most 32 MCP locations and 32 connections, including retired
connections still held by a caller. Session configuration revisions and subagent
worktrees have separate location keys. Active turns, startup attempts and bound
StateEngine producers keep their location; idle unprotected locations give way
first. If all locations are occupied, a new turn stays queued with a capacity
message. An unsent call whose location was released can re-admit once from current
configuration within its original deadline. A changed or disabled declaration
refuses the old route; a call that started an exchange is never repeated by this
recovery.

Each release writes an `MCP_LOCATION_EVICTED` server-log line with the location,
Unix timestamp (`at_ms`), last-read age (`last_read_age_ms`), and bound-producer
flag. It contains no launch environment or tool arguments.

A bound StateEngine tool that explicitly refuses a stale producer retires that
connection. Grapt retries `stateengine_msg_read`, `stateengine_msg_list` without
dismissal, or `stateengine_resume` without a packet acknowledgement once, within
the original deadline and approval. Persistent refusal returns an error. Writes,
acknowledgements, dismissal and other tools are never transparently repeated.

## Server-requested input and URL consent

A configured user-owned or explicitly approved project server can ask for input during
initialization or tool discovery, before a Session is selected, or during a tool
exchange. These questions are durable **global forms scoped to the resolved
repository location** and do not require a selected Session. Basic forms retain
their existing behavior; URL consent requires negotiated MCP 2025-11-25.

### Answer in the native R TUI

Attach to the same service profile and repository, then use `/mcps` or
`/mcp verbose` to inspect configured servers. Inspection remains pending without
blocking question reads or answers. Press **F6** to open questions, including
startup questions that arrive before you create or select a Session.

For a URL request, inspect the requesting server, host and full literal URL;
**PgUp/PgDn** scroll the details. Use the choice controls (arrows, then Space) to
select **Accept (continue manually)** or **Decline**, and **Ctrl+S** to submit.
**Ctrl+R** sends **Cancel**, which is distinct from Decline; **Esc/F6** only closes
the pane and sends no decision. For a basic form, fill its fields and submit with
Ctrl+S. If delivery is uncertain, **Ctrl+O** reads the recorded state before you
explicitly retry the original decision.

Unlike `/mcp login`, URL consent never opens a browser, fetches the destination,
or performs sign-in for you. Navigate manually only if you trust the server and
destination. Accept means consent, **not** that a browser opened or the external
interaction completed. Keep URLs private; do not paste their sign-in state or
secrets into the model conversation.

### Headless service and other clients

An ordinary headless command has no startup question recipient: without a live
eligible attachment, startup elicitation is refused promptly rather than
approved or left waiting. A custom operator client must first keep an
**authenticated**, matching-location `/api/event` subscription open with
`X-Grapt-Approver: *`; an observer or a Session-only approver is not sufficient.
Use the same authentication and `location[directory]` selection to:

- Read questions with `GET /api/form/request`.
- Submit URL consent to `POST /api/session/global/form/{formID}/reply` with
  `{"answer":{"decision":"accept"}}` or `{"answer":{"decision":"decline"}}`.
- Cancel with `POST /api/session/global/form/{formID}/cancel` (no body).
- Read an uncertain outcome with
  `GET /api/session/global/form/{formID}/state`.

Use the returned local `formID`, not the server's separate `elicitationId`.
Basic forms use the same reply endpoint with their field answers. The native R
TUI supplies this attachment and these form operations itself.

### Bounds and current limits

Startup human waits share a cumulative **ten-minute allowance** across the
catalog operation. Human time does not spend the configured active startup and
catalog budgets or the enclosing recovery allowance. Native catalog acquisition
keeps a finite human allowance separate from active time; nested CodeMode
execution does so only for startup owned or joined by its active nested MCP calls
on the requested server. Unrelated server inspection cannot extend a VM's cap.
Repeated questions do not renew these budgets. Disconnect,
retirement, cancellation or process loss ends the live wait and may leave a
suspended form; reopening it does not recreate that server callback or replay a
tool.

Requests are processed during active exchanges, not by an unsolicited idle
request dispatcher. Completion notifications do not mark external work complete.
A URL-required `-32042` error remains a bounded, redacted refusal: its interactive
presentation/manual-retry UI is not implemented, and no tool is automatically
replayed.

## Workspace roots

MCP servers may request the current workspace roots with `roots/list`. The
response contains `file://` URIs for that scope's workspace and any additional
directories declared in user configuration:

```json
{"mcpRoots": ["/absolute/path/to/shared-project"]}
```

Up to 32 additional directories are supported. Repository configuration cannot
widen this list, and unapproved project servers cannot connect or receive roots.
Roots describe the workspace; they do not grant filesystem access or replace
tool permissions.

A server that has requested roots receives `notifications/roots/list_changed`
when its scope's roots change. Opening another Session, including through
`/worktree open`, uses that Session's own approved scope. Selecting it never
repoints another Session's connection. Servers that do not request roots receive
no roots notifications, and roots do not change the model's tool catalog.

## Tool progress

MCP tools can report progress while they run. The active tool row shows the
server message, or a percentage when the server supplies a total. Updates
are limited to twice a second and disappear on completion or reconnect.
Progress is display-only and does not enter conversation history or model input.

## Resource updates

Use `/mcp resources <server>` to browse, then `/mcp read <server> <uri>` to add
resource content to the conversation. If the server supports subscriptions, a
read also watches that resource for this Session. `/mcp pin <server> <uri>`
watches without reading; `/mcp unpin <server> <uri>` stops watching.

The `/mcp` pane marks changed resources **updated** and offers a re-read command.
Updates never fetch new content or send a model prompt automatically. Re-reading
acknowledges the update only in the selected Session. Each server connection can
watch up to 32 resource URIs. Watches end when unpinned, when their Session ends,
or when the connection closes; read or pin again after reconnecting.
