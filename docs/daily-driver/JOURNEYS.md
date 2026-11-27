# Walk the packaged native TUI

Use the Windows and Linux packages from the same source commit. Record the
`engine_version` in each `release-manifest.json`; the two must match. Run the
package's `grapt --version` and retain its build line. Use every binary from
that package, rather than mixing it with a checkout's debug binaries.

Use a disposable working clone of a real repository and a new private profile
outside it. Keep the task small enough to review its actual diff. For this
project, a useful task is to inspect one current user-facing command, correct a
specific missing instruction, add its ordinary regression check if needed, and
run the affected tests. Do not use the paused ACP root or a daily-driver profile.

Start from the repository, using the package's absolute executable path:

```text
grapt code --setup <approved-existing-account-directory> --profile <new-profile> <working-clone>
```

`--setup` imports the named account inputs into a new profile without a provider
call. Use the already authorized Codex account/model. Do not sign up, use the
held Claude subscription, or enable another paid account. A genuinely fresh
interactive account setup remains a separate J01 manual observation; importing
an account is not that observation.
A setup timeout may leave a partial new profile; preserve it, and use an unused
destination for `--setup`.

Choose the repository, agent and model, then use `/new` for a new Session.
Record its Session ID. Ctrl+P opens saved Sessions, F7 agents and F3 models.
Escape closes a picker or, from the composer, interrupts work or opens Sessions.

| Journey | Operate the actual package | Record |
| --- | --- | --- |
| J01 | Observe first-start choices or import the approved account, then reach the repository/account/model pickers. | Distinguish fresh setup from import; build and platform. |
| J02 | Ask about a real function; have it search/read, make the agreed small edit, run affected tests and show `git diff`. | Task, actual files changed, test result and visible diff. Nonzero diff exits retain their output. |
| J03 | Watch tool activity. Answer a real permission request only after reading it. Exercise a recoverable mistake, such as `/rename` without a title, and then correct it. | Tool/permission display, actual outcome and useful error text. A menu alone is not a permission-effect check. |
| J04 | During a task, Ctrl-C; verify the task stops and any canceled permission pane disappears. Submit a smaller redirected request and finish it. | Interrupted task, response to Ctrl-C and completed redirected request. A queued input alone does not prove steering. |
| J05 | Use `/quit` or double Ctrl+C within 1.5 seconds, run the same command with `--profile <profile>` and **without `--setup`**, and reopen the same Session. | History and saved edit intact; interrupted effect absent; no duplicated task/effect. |
| J06 | Continue useful multi-step work at unchanged defaults. If automatic compaction occurs, finish useful work afterward. Keep the already measured `/compact` path separate; do not change thresholds or add filler to force a trigger. | Record the actual trigger and useful completion; requested compaction, scheduling or a busy footer does not prove automatic compaction. |
| J07 | Read the build identity. Use the already reviewed update/revert journey with a fresh disposable prefix/profile when authorized. | Actual installed versions and preserved Session. Do not change the live install or infer update success from package checksums. |

The prompt footer advertises send, interrupt and quit first. Shift+Tab focuses
visible tools; Tab cycles them and Enter or Space expands bounded details.
PageUp/PageDown scroll the transcript. `/mcps` shows
MCP status, `/rename <title>` names the Session, and `/compact` schedules
compaction. Selected permission preset and effective trust remain separate;
running work keeps its captured policy.

Retain terminal captures and the small reviewed diff. Record PASS, FAIL or
UNMEASURED for each platform in `RESULTS.md`, with the concrete outcome and
remaining owner. Interactive Windows checks that need a person at the desktop, and
daily-use confirmation, stay manual until performed. Automated mechanics, source fixes and a
successful Linux walk do not certify those rows. Quit with `/quit` or double
Ctrl+C and verify the owned product stops; preserve the scratch profile for the resume check.

For the author, build both packages after the quiet benchmark handback:

```sh
bash scripts/package/build_daily.sh /absolute/new-output /absolute/pinned-node-cache
```

The cache contains the archives and extracted directories for the version in
`packages/plugin-bridge/runtime-pins.json`. The existing packager validates those
assets. The builder uses the shared cache/host lock and unchanged release tools;
it does not download runtimes, sign artifacts or declare journey acceptance.
