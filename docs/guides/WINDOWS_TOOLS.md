# Windows tool smoke

`scripts/stateengine-native/r_windows_tools.py` runs a disposable coding workflow
with the packaged R frontend and a same-source Product built with
`loopback-provider-testkit`. The model is a local scripted provider; commands,
patches, undo, compaction and persistence use the production tool paths.

The workflow checks:

- A background PTY command compiles Python code and tests signed addition;
  output polling retains its result and successful exit.
- A second command owns a child process. One Ctrl-C followed by output polling
  settles the command, drains its output, and proves both recorded process
  identities exited before fixture cleanup.
- Codex text patches update, move, add and delete Unicode paths. Existing CRLF
  files retain their line endings; added files use LF. Native `/undo` restores
  the original bytes without changing conversation history.
- A mixed-status plan survives compaction, completes through `update_plan`, and
  remains visible after restarting Product and R without another model call.
- Project `AGENTS.md` instructions, an installed skill body and decoded image
  pixels reach the model. A public HTTPS fetch and calls through actual stdio
  and remote HTTP MCP peers return their checked results.

Native web search is advertised and synthetic search framing is sent by the
scripted provider. Retained search replay and an external provider search remain
unmeasured. These limits are recorded separately; this does not close the
real-search part of release criterion 6.

Run a startup/exit smoke before each full journey. Hold the shared PC controller
lock for every invocation. Use a new `run-*` directory for each attempt beneath
`E:/temp/win-tools-<source-prefix>/`, with at least eight source characters:

```powershell
python scripts/stateengine-native/r_windows_tools.py `
  --package E:/temp/win-tools-<source-prefix>/package `
  --fixture E:/temp/win-tools-<source-prefix>/fixture `
  --source <full-source-commit> `
  --node E:/temp/win-tools-<source-prefix>/package/plugin-runtime/node.exe `
  --node-modules <retained-observer-node_modules> `
  --backend system --scratch E:/temp/win-tools-<source-prefix>/run-system-smoke --smoke
```

Repeat without `--smoke` in a fresh directory, then smoke and run with
`--backend modern`. The shared carrier uses pinned `@lydell/node-pty`
1.2.0-beta.12 and `@xterm/headless` 5.5.0. The journey checks the observer's loaded
`conpty.dll` to distinguish system ConPTY from the bundled modern backend.
Keep the matching fixture Product and `source.txt` together. Stage the harness's
`scripts/` and `fixtures/stateengine-native/` dependencies with their relative
paths preserved.

`steps.json`, `steps.md`, terminal captures and `model.json` stay outside Git.
Failed attempts retain FAIL and NOT RUN rows. Offscreen transport checks do not
establish visible conhost/Windows Terminal behavior, performance or a real
producer seat-day. Current results qualify these two transport paths on the
pinned package; repeat after a relevant tool or platform change.

Windows shell jobs stop ordinary descendants when their command ends or is
cancelled. Programs that explicitly request Windows breakaway can start a daemon
that outlives the command, such as `se up --detach`; close that daemon through its
own shutdown command. An outer job imposed by your host can still refuse breakaway.
