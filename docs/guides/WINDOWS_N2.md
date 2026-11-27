# Background sessions and worktrees on Windows

A background tool keeps running after its first output page. Poll its returned
terminal handle; restarting the service expires that handle, but retains the
completed tool output and conversation history. `spawn_agent(worktree=true)`
creates a detached Git worktree from committed files and preserves the parent’s
nested working directory. Uncommitted parent edits stay in the parent checkout.

Product worktrees are retained. Before removing one, save its output and stop
processes using its files. On Windows an open file can make Git removal fail
after Git has already unregistered the checkout. Inspect `git worktree list
--porcelain` before retrying. Remove only the owned residual directory after its
handles close; inspect junctions with `cmd /d /c dir /a:l <checkout>` and remove
the links themselves without traversing their targets.

## Run the native N2 regression

The regression uses the shared Windows ConPTY carrier and a local fixture model.
It runs R and the service natively with isolated profiles below a fresh
`E:/temp/theta-*` directory. It makes no paid provider calls and does not use
StateEngine binding, mail, port 4877, system TLS or producer profiles. A synthetic
pass does not establish a real seat-day, cache share or release performance.

Requirements:

- Windows with ConPTY, Git for Windows, Python 3.14 and Node.js.
- Node packages `@lydell/node-pty@1.2.0-beta.12` and
  `@xterm/headless@5.5.0` in the directory supplied as `--node-modules`.
- Matching Windows fixture executables `grapt.exe`, `grapt-server.exe` and
  `grapt-tui.exe` in one directory, with their full source commit in
  `source.txt`. Build with the features below; ordinary release binaries refuse
  this loopback fixture configuration.

From the Linux build host, build the fixture executables with the pinned MSVC
cross toolchain:

```sh
bash scripts/shared-cargo.sh xwin build --locked --release \
  --target x86_64-pc-windows-msvc \
  -p grapt-cli --bin grapt -p grapt-product --bin grapt-server \
  -p grapt-tui --bin grapt-tui \
  --features grapt-cli/loopback-testkit,grapt-product/loopback-provider-testkit
```

Copy the matching executables and `source.txt` to the private Windows fixture
directory. Copy `scripts/` and `fixtures/stateengine-native/` from the tested
checkout, preserving their relative paths; the shared carrier imports helpers
from `scripts/native-tui/`.
On Windows, first run the startup/exit smoke, then the complete journey with a
different fresh root. Replace `<source-commit>` with the full commit from
`source.txt`; keep the scratch name at least as long as this example so the
worktree fixture exercises file paths longer than 260 characters.

```powershell
python scripts/stateengine-native/r_windows_n2.py `
  --binaries E:/temp/n2-binaries --source <source-commit> `
  --node-modules E:/temp/n2-observer/node_modules `
  --scratch E:/temp/n2-native-smoke --smoke

python scripts/stateengine-native/r_windows_n2.py `
  --binaries E:/temp/n2-binaries --source <source-commit> `
  --node-modules E:/temp/n2-observer/node_modules `
  --scratch E:/temp/n2-native-journey
```

`result.json` distinguishes smoke from a complete pass. `steps.md` and
`steps.json` report every required step, including FAIL and NOT RUN; exact process,
HTTP and input commands are retained in `commands.jsonl`. Keep these outputs
outside Git. `--phase composer` and `--phase worktree` isolate a failing half;
either is only a partial journey.

The complete journey checks:

1. Native ConPTY startup and an isolated fixture Session.
2. Real background output, explicit Once approvals, edited queued input and
   automatic compaction.
3. The same R process reconnecting after a hard kill, preserving its unsent draft.
4. Tested work after restart, with exact input and approval identities and distinct
   background terminal handles.
5. Detached child worktree creation with spaces, Unicode, nested directories and
   file paths over 260 characters (`core.longpaths=true`).
6. A real background child writing only its checkout and preserving CRLF bytes.
7. Completed child output, lineage and placement surviving a hard-kill restart
   without replaying the command.
8. Cleanup refusing an independently held file while preserving an external
   junction target.
9. Cleanup succeeding after the holder exits, preserving the dirty parent
   checkout and external target.

The final two steps are explicit operator cleanup of this disposable fixture,
not an automatic product deletion policy. The fixture also handles Git’s partial
unregistration on a failed Windows removal.
