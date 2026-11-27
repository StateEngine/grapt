Grapt's native frontend uses the existing Grapt protocol client.
Build with `cargo build --release -p grapt-tui` and attach to a prepared session:

```sh
grapt-tui --service PROFILE --directory REPOSITORY --session SESSION_ID --maintainer
```

The service profile uses the same readiness receipt and credential as
`grapt-tui`. Dark is the default theme; pass `--light` or press F2 to switch.

- Ctrl+A or `/connect` opens provider connections. A fresh profile opens it automatically.
  Enter an API key, or choose OpenAI → ChatGPT browser/device sign-in. Anthropic uses API keys.
  Ctrl+O opens the sign-in page; Ctrl+Y copies its link. Esc cancels sign-in.
  Connections can be renamed or disconnected there. F3 lists connected providers' models.
- Enter sends; Shift+Enter inserts a newline. The draft stays until submission succeeds.
- Ctrl+R searches complete prompts in the loaded history. Type to filter;
  Ctrl+R or Up finds an older match, Down a newer one. Enter accepts for editing;
  press Enter again to send. Esc or Ctrl+C cancels search and restores the draft.
- Up/Down recall earlier prompts when the editor is empty or at a recall boundary.
- Ctrl+P opens the command palette. F1 or `/help` lists actions and shortcuts.
  Choose Switch session in the palette or use `/resume`; Esc also opens sessions while idle.
  Type in the session list to search saved titles on the server. Local matches stay
  visible while searching; a failed search is labelled. Use arrows, PageUp/PageDown,
  Home/End and Enter to choose. Ctrl+G switches between updated-date (UTC) and
  directory groups; missing metadata is labelled unavailable.
- In sessions, Ctrl+B pins or unpins the selected row. Pins persist in
  `PROFILE/session-pins.json`; the first nine pins use leader+1 through leader+9
  (Ctrl+X by default). These slots also appear in the palette and can be rebound. Slot defaults yield
  to existing custom shortcuts; conflicting explicit bindings are rejected.
  Ctrl+R edits a title inline; Enter saves and Esc cancels without changing the composer.
- Ctrl+D opens deletion confirmation for the selected session. Enter requests engine
  deletion, including stopping its running work; Esc cancels. A refused or unconfirmed
  deletion keeps local history visible. Confirmed deletion removes the row and its pin.
- The cache line shows the last request and weighted session assistant-history read
  shares. A dash means unavailable, including incomplete history or unreported
  input/cache reads; zero input has no percentage. F4 opens full live-session usage.
  Use arrows, PageUp/PageDown or Home/End to review; Esc or F4 closes it. Browsing
  older messages keeps this status on the current selected session.
- F3 opens model and effort choices. The header shows the current selection.
  Type to filter; Enter selects, Esc closes. Disabled choices remain visible.
  Selection failures preserve the current model and draft.
- Edit, patch, apply_patch and write results show inline file diffs. Each file
  starts folded to ten lines; click its heading/fold row or press Ctrl+O to toggle
  the latest visible file. Shift+Tab and Tab focus individual files. Removed/added
  lines use red/green fills in both themes. `?` marks unavailable counts or line offsets.
  Large diffs show up to 256 lines per file and label omitted source.
  Rejected edits show the attempted diff; inspect files after a partial failure.
- PageUp/PageDown scroll the transcript; Ctrl+Home/Ctrl+End jump to beginning/latest.
- Alt+Down cycles the current session family. Its status panel appears during
  a run and collapses while idle.
- Provider retries appear above the transcript while the runtime waits; they run
  automatically. Esc interrupts a retry or active run when no overlay is open.
  Terminal failure and interruption notices keep the edited draft. Review the
  retained history and send a follow-up to continue with completed tool results.
- Native inbox failures keep `mail: offline` in the status line independently of
  the service connection. Recoverable failures retry automatically with backoff;
  authentication or takeover refusals still require the StateEngine binding path.
- Alt+R checks an uncertain submission using its original prompt identity. It does
  not replace an edited draft or automatically repeat completed tools.
- Built-ins keep their meaning: colliding custom commands are named
  `/project:review` or `/user:review`. Project templates require an approved,
  unchanged repository profile; unavailable commands show their reason.
- Custom commands show their project/user source path and expanded text before
  sending. Esc closes the preview; Enter in the composer confirms the staged
  text. Clear the draft to cancel. Every invocation needs confirmation.
- Type `/` to complete built-in, project and user commands; `/commands` reloads
  the server catalog. `/review [commit|branch|pr]` reviews changes, defaulting
  to uncommitted changes. Command arguments support quoted groups.
- `/rename <title>` renames the selected session. `/undo` previews file restoration;
  close the preview, then `/undo apply` restores those files without changing the
  conversation. `/rewind [1–200]` reopens an earlier prompt without changing files.
- Unknown slash commands stay in the composer. Use `//text` to send `/text`
  literally. Catalog failures keep the draft and offer `/commands` to retry.
- `/clear` starts a fresh session with the selected agent, model and effort;
  saved conversations remain available through `/resume`. Extra arguments keep
  the current session and draft.
- `/quit` exits. Ctrl+C interrupts or clears the draft; a second press within
  1.5 seconds exits.
- Permission previews scroll with PageUp/PageDown. Use y for Once, s for Session,
  a for Always or n to Reject.

Native action shortcuts can be configured in `tui.json` inside the service
profile (`--service`). The file is read at launch; `/help` and the command
palette show the effective shortcuts. `/palette` opens the palette even if its
shortcut is disabled. For example:

```json
{
  "keybindings": {
    "leader": "ctrl-x",
    "timeout_ms": 1000,
    "bindings": {
      "palette": ["ctrl-p", "leader p"],
      "sessions": "leader l",
      "models": "f9",
      "help": [],
      "quit": "leader q"
    }
  }
}
```

Available action names are `palette`, `connect`, `sessions`, `session_slot_1` through `session_slot_9`, `models`, `agents`,
`usage`, `theme`, `questions`, `help` and `quit`. Ctrl+A stays fixed as Connect;
omit `connect` from overrides. Ctrl+C retains interrupt, draft clearing and
double-press exit. Other actions accept one key, a two-key chord, or up to eight
alternatives. `[]`, `false` or `"none"` disables a shortcut; slash commands and
palette entries remain available. Prefix keys use Ctrl, Alt or F1–F24 so ordinary
typing stays in the draft. `+` and `-` separate modifiers, and `<leader>p` is also
accepted. The default leader is Ctrl+X, with `leader l` opening sessions and `leader 1` through `leader 9` switching to pinned sessions.

Esc cancels a pending chord. A missed or expired completion returns to normal
input, and changing sessions, pasting, clicking or losing focus cancels the chord.
The timeout is 100–10000 ms. Text editing, picker navigation, question answers,
permission replies, startup recovery and Connect input retain their modal keys.
Inside questions, F6 always returns focus to the composer; field editing keys
take precedence over configured chords.
Malformed files, unknown actions and ambiguous shortcuts report an error before
the terminal opens. Settings are bounded to 64 KiB.

Input and protocol processing use separate bounded queues. Rendering merges
stream updates at up to 60 frames per second; input preempts a pending frame.
It retains completed transcript layouts and only
paints visible cells. Streaming assistant lines appear at newline or block
completion; reasoning and tools appear as complete lifecycle cells.

Use `/model` or `/effort` to open the model/effort picker.
`/model provider/model [effort]` selects an offered catalog entry directly.
`/effort level` changes the current model’s reasoning effort; `default` restores
the provider default and `status` shows the effective selection. Unsupported
models, efforts and arguments keep the draft and selection unchanged.

Use `/permissions` to read the selected and effective server policy, or select
`inspect`, `author` or `maintainer` by name. Configuration may narrow the effective
policy. Bypass requires the explicit `/permissions bypass confirm` form and shows
a persistent warning. Invalid choices keep the draft and do not change permissions.
If a selection response is lost, sending and further permission changes stay blocked
until the session is reopened; a read alone does not confirm the earlier write.

The palette searches native actions and loaded workspace commands. Actions that
need arguments insert an editable command into an empty draft; existing drafts
and images stay intact. Unavailable actions explain what is missing.
`/sessions` and `/continue` alias `/resume`; `/models` and `/mo` alias `/model`;
`/variants` aliases `/effort`; `/preset` and `/trust` alias `/permissions`;
`/summarize` aliases `/compact`; `/exit` and `/q` alias `/quit`.
`/agents` opens the offered-agent roster. Custom commands that collide with these
aliases remain available with their `/project:` or `/user:` prefix.

Compaction completes as a single collapsed **◇ Compaction completed.** row. Click the row, press Ctrl+O, or focus it with Shift+Tab and press Enter to reveal or hide its summary and reported metadata, including retained context and supplied token counts. Running compaction records stay as one live status row; the timer measures time observed in this TUI. Missing metadata stays absent.

Open **Settings and preferences** in the command palette, or run `/settings`. Search by name or description, use ↑/↓ to select, and Enter or → to change; ← decreases wheel speed. Color mode, notifications, copy on selection and wheel speed save immediately in the profile’s `display.json`. Failed saves keep the previous active value. Esc closes settings and preserves your draft.

Explicit `--light`/`--dark` and `--no-notifications` override saved values at startup. F2 remains a temporary theme switch; selecting a color mode in Settings saves it for later launches. Settings edits affect this running TUI and later launches; they do not change engine permissions or provider configuration.
