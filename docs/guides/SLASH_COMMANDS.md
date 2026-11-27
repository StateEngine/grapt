# Composer slash commands

Slash commands are typed into the composer and sent like any line. A line
that matches a command is executed by the TUI itself and never becomes a
model prompt — with one exception: `/init` sends a prepared prompt, and any
line that matches no command, including an unrecognized `/word`, is sent to
the model as an ordinary prompt. Commands match the trimmed line on word
boundaries, so `/compactx` is a prompt, not `/compact`. `/help` opens the
in-TUI reference of keys and all commands.

## /status — show session status

Opens the STATUS pane, read-only and navigable; Escape returns to the
composer. It reads the session's current projection, not a cached copy, so it
is re-read after a fresh history open. Rows: Model, Effort, Account, Context
(input over capacity with percent used), Tokens and Cache from the provider's
last reported usage, optional Calls and Miss rows, and Session (the selected
session id, or `none`). A row the provider did not report says `not
reported` or `unavailable: <error>`; a failed account read never reuses a
previous label. The account is the selected provider's, not the primary
one's.

## /model — open the model picker

Opens the model and effort picker instead of submitting a prompt. A choice
made from the picker is the session's selection and survives reopening a
fresh history. `/model` takes no arguments; anything after it earns the
usage notice and is not sent to the model.

## /compact — compact before the next message

Admits a compaction that runs before your next message; it sends no prompt.
The notice says "Compaction will run before your next message.", and
reopening history neither resubmits nor forgets the admitted compaction.

## /new — start a new session

Starts a session with the selected agent, model and effort; those
selections are preserved, and both the old and the new histories reopen
independently. `/new` takes no arguments: with arguments it creates no
session and keeps your draft with the usage notice.

## /resume — resume a saved session

`/resume` with no arguments opens the session picker; Enter opens the chosen
session and restores its saved choices. `/resume <session id>` opens
directly, including history outside the visible session page.
`/resume <title>` opens when exactly one session has that title; a title
held by more than one session asks for the id, and a missing target says the
session was not found — in both cases the command stays in the composer and
the current selection is unchanged. Resuming never submits a prompt. If the
session has no selected agent or model, focus moves to that picker instead of
the composer.

## /export — save selected session history (R TUI)

`/export [markdown|json] [path]` writes a new file, never the clipboard.
The default is Markdown and the default filename is
`<selected-session-id>-export.md` (or `.json` for `/export json`), relative
to the terminal process's working directory. To choose a path, give the format
first: `/export markdown notes/session.md` or `/export json "session archive.json"`.
The rest of the line is one path; spaces are supported, matching outer quotes
are optional, and shell variables and `~` are not expanded. Parent directories
must already exist.

Export reads all public history pages in source order, not just the visible
window. JSON contains `sessionID` and `data` (the public message DTO rows).
Markdown preserves raw text, reasoning and tool order, with structured JSON
blocks for tools and other session records. Export does not change the
selected session, the reading window, or submit a prompt. For a stable archive,
wait for the response to finish; this is a paginated read, not a transactional
snapshot of an actively changing conversation.

Unreadable or incomplete history, cursor cycles, repeated rows and bounds
exhaustion refuse the export without a destination file. Reads are limited to
200 rows per page, at most the client's page budget (capped at 2,000 pages),
100,000 rows and 64 MiB of encoded pages; output is limited to 128 MiB.
Existing paths (including symlinks) are refused. Complete output is staged
beside the destination and published without replacement; the filesystem must
support hard links. A leftover `<path>.grapt-export-tmp` from an interrupted
process is never overwritten: inspect/remove it or choose another path.
Exports may contain sensitive prompts, reasoning and tool output.

## /diff — inspect the current workspace

Opens the WORKSPACE DIFF pane and reads the workspace now: staged, unstaged
and untracked changes. Outside a Git repository it says "Not inside a Git
repository."; a clean tree says "No staged, unstaged or untracked
changes."; an unreadable or failed read says so instead of showing an empty
pane. `/diff` takes no arguments.

## /mcp — inspect MCP servers or tool details

`/mcp` opens the MCP pane, read now on the location's current truth rather
than on a stale read: the servers, their status, and tool counts. Enter on a
failed or needs-registration row shows its error in the frame. `/mcp verbose`
opens the details view listing tools and withheld names. `/mcps` is the
legacy form of `/mcp` (the list view). The pane is read-only: pasting into
it is refused because it has no search. MCP sign-in is not available in this
view — `/mcp login` says so, and any other argument earns the usage notice;
neither becomes a prompt.

## /init — prepare repository guidance with the agent

Requires a session first: without one it says "Create or resume a session
before /init." and sends nothing. With a session, `/init` sends a prepared
prompt — the only command that does — asking the agent to write an AGENTS.md
contributor guide: a concise "Repository Guidelines" document (200–400
words) covering project structure, build and test commands, style and
naming, testing, and commit conventions, adapted to the repository. The
prepared prompt instructs the agent to check for an existing AGENTS.md
first and not to overwrite or modify it. If the model's work is refused,
the task stays editable with no automatic retry. `/init` takes no
arguments.

## /fork — branch a conversation

`/fork` opens a saved child containing the conversation so far. `/fork n`
branches before the nth most recent prompt (1–200) and stages a copy of that
prompt for editing. Nothing is submitted until you send it. The parent and
workspace files stay unchanged; use `/resume` to return to the parent.

## /side — ask without saving a turn

After a response finishes, `/side <question>` asks the selected model using a
snapshot of the conversation and its instructions. The answer opens in a pane;
Escape returns to the composer. The question and answer are kept out of saved
history, and no child appears in Sessions. Later parent prompts do not inherit
the side exchange. Side questions offer no tools or file effects.

Live events and editing continue while the answer is pending. Switching Sessions
cancels the pending answer. A failed request keeps the draft and is not retried
automatically. Questions and answers are limited to 64 KiB each; an unreadable,
unsettled or oversized conversation refuses the question rather than dropping
context. Side questions use the selected model and its normal provider billing.
