# macOS support and CI

The daemon admits the exact operating-system names `windows`, `linux` and
`macos`. The platform-parity ruling of 2026-10-19 supersedes the earlier
ruling that excluded macOS. Every other name is refused before sockets,
locks or registration reads; a Unix-family match does not grant support.

The native arm64 workflow checks every workspace crate, including the daemon.
It runs manually, with two build/test workers and
a 15-minute job limit. Diagnostics and per-suite logs are retained for 14 days.

Product coverage is deliberately bounded:

- Daemon argument, platform, custody, receipt and status tests exercise actual
  loopback serving and filesystem receipts, rather than compilation alone.
- CLI library unit tests cover argument handling, client behavior and launch
  contracts. An empty test selection fails CI; the shipped CLI is built
  separately for the product journeys.
- Store tests prove writable SQLite access and real sidecars. Three native
  pathname-swap witnesses run before and after removing the final binding
  check; all three must fail under that mutation and pass after restoration.
  Each phase has a two-minute limit and an 8 MiB log bound. The helper restores
  source even on failure and kills only its own subprocess group.
- Four headless journeys run the real product with a loopback provider: live
  streaming, usage and restart; patch approval and file undo; cache-prefix
  continuity; and crash/resume through the shipped CLI with mismatch refusals.
  Each selected journey must report exactly one passing test.
- Skills watcher controls cover registration and rearming, edits across locations
  during registration, and rejection of non-Unicode watch paths. Both named
  controls must run, with no failures or ignored tests; their log is retained.
- All plugin targets run with Node 24.21.0, including native backend, bridge,
  envelope and lifecycle tests. `NODE` names the verified pinned executable;
  `/bin/sh` and `/usr/bin/perl` must also exist for the native fixtures.
- Pure type, event and policy tests remain enabled. The plugin suite is strict:
  an absent or failing macOS backend fails CI; it is not an allowed failure.
- The Seatbelt candidate supplies the native adapter smoke: offline cargo
  build/test, npm installation and postinstall, and a linked-worktree Git
  commit. Protected Git and application files, outside writes, and network
  access with networking disabled must be refused. Removing the protected-path
  guard must expose the Git-config escape; restoring it must pass again.
  Both pipes and PTY must preserve the root's exit status and leave no live
  descendant after that exit.

Platform-specific custody or receipt failures remain failures to investigate;
the workflow does not normalize or waive them. The Linux-specific daemon
operator-interrupt witness remains identified as such by its existing test.

Approved macOS candidates can run as temporary `ci/<item>` branches in the
private repository. Their diff from remote `main` must leave `.github/`
unchanged, and publication requires the same incremental secret scan and
triage. The owner of CI dispatch publishes the exact candidate, runs at most
twice per branch per day, and deletes the remote branch after its final run.
Workflow changes reach these candidates through reviewed `main`.
