# Build, review and land

The designated merger can compose reviewed pull requests with
`python3 scripts/build/batch_land.py NAME --project OWNER/REPOSITORY --pr NUMBER
--repository /absolute/controller --state /absolute/external/attempt`.
Repeat `--pr` for up to eight candidates. The default prints the steps; add
`--execute` to start a detached controller. Supply `--actor REGISTERED_AGENT`
and one `--author NUMBER=REGISTERED_AGENT` for every PR; shared GitHub identities
are never guessed as seat owners. Phase results go to the registered coordinator
and these explicit authors. `--worktrees` selects an existing directory outside the
controller. Configure `--mirror-host` and `--mirror-path`, or the corresponding
`SECODE_WINDOWS_HOST` and `SECODE_CANONICAL_REPOSITORY` operator settings.

The controller reads an explicit verdict from the opening review paragraph,
such as `ACCEPT at <commit>` or `REQUEST CHANGES`. Quoted, contradictory or
ambiguous verdict text cannot grant acceptance. An ACCEPT must
cover the exact head, except for mechanical merges of main with no manual edits.
It preserves publishable history with a merge commit and squashes the entire PR
when its history fails the publication scan. A squash commit records the landed
PR number; invalid final content still refuses publication. Only independent module-budget keys
and the generated license-input digest are resolved automatically; other
conflicts exclude the PR. License text is never selected by choosing a side.

Each attempt pushes a fresh `staging/NAME` ref and runs locked Cargo metadata,
the affected gate with `--precheck --staging` against the exact base, and the
license check. Staging runs whole touched-crate suites, scoped Clippy, and all four
laws; core changes do not widen this early check to the workspace. Script changes
also run the build-control tests; CI changes run the CI helper tests. The gate
owns its scheduler lease. Its elapsed staging time is retained in the checkpoint.
It then pushes `batch/NAME` and waits for that exact commit's CI. Later attempts
append a numeric suffix; an existing ref is accepted only at the identical head.

A failed test report must match the exact workflow run, attempt, head and base.
Only a test in a crate touched by exactly one included PR can directly exclude
that PR. Missing, shared or ambiguous failure evidence falls back to qualifying
the base and growing a green prefix. A passed-on-retry test is reported without
exclusion; a final quarantined failure remains real. When that failure is outside
every touched crate, the controller stops and notifies the coordinator and its
quarantine owner instead of running prefix gates. Network errors never identify
a culprit. Batch test reports are retained as bounded CI artifacts for seven days.

After qualification, unchanged reviews and heads are checked again. Main must
still be at the composed base before the normal fast-forward push. Included PRs
that GitHub did not mark merged receive a landed-SHA comment and are closed,
after checking their heads. The Windows
mirror must already be clean and on main; it fetches and merges with `--ff-only`.
Keep the external attempt directory and owned checkouts after a failure.
Use `--resume` instead of `--execute` to continue the saved attempt after a
disconnect or failure. `--resume-mirror` remains available after publication.
The controller checks remote main before retrying an uncertain push and discovers
already-posted comments and closures. Failed phase mail remains explicit delivery
debt. A stable recipient delivery key survives lost acknowledgements; resuming
delivery never repeats canonical publication. Only one controller
can own a checkpoint directory at a time. The durable launcher prints the job
identity and retained output log, independently of the invoking seat's lifetime.
Selecting the candidates, obtaining their required independent reviews, resolving
unsupported conflicts, and reconciling uncertain publication remain operator work.

Normal scheduled full qualification runs Linux and Windows-target checks, followed
by the final hosted `check` with its Windows obligations. Without `--native-windows`,
the local gate skips the optional native Windows sidecar without preparing or
launching it, even when native packages are touched. Supplying that option requests
the existing governed native route, including its source identity, cancellation
and result checks; a selected sidecar replaces the local Windows-target leg.

Landing batch gates pass `--batch`. They may omit xwin only when every changed
path is provably inert: documentation, or a box-only script that never ships or
compiles. Any Rust source, manifest, toolchain or Cargo input, workflow,
packaging script, lint configuration, or path this tooling does not recognise
runs the checks, so the rule fails toward running. An empty change set counts as
unknown and runs them too. Nightly always includes xwin. Verifying a batch
receipt also requires `--batch`; an ordinary receipt cannot silently acquire
that policy.

A disk-pressure warning can leave a retained admission ceiling. Its stored
reason describes that earlier warning; current free-space and pressure samples
remain separate admission inputs.

The scheduler admits up to twelve ordinary jobs plus one reserved landing slot,
subject to measured memory and pressure. It retains class/FIFO order among requests that can use the
available placement; it never terminates running work to make space. NVMe and
governed RAM jobs can share the heavy flock; other disk work remains exclusive.
Old waiting clients keep their original three/four-slot admission rules and v1
records. A current client can use further hardware capacity while an older
client waits for a slot it understands. Protocol support is recorded separately
from resource profiles, so a missing profile does not restrict a current client
to four slots. Existing queued launchers retain their version and queue age.

Admission keeps 16 GiB of available RAM after the candidate's projected peak and
any unmaterialized growth of admitted jobs. `MemAvailable` already accounts for
tmpfs: the scheduler never subtracts `Shmem` again. RAM-gate memory already
allocated is credited only from its cgroup governor. A shared lease whose launcher has died is conservatively priced at the full 24 GiB RAM ceiling until its last child releases it; no unverified RAM usage is credited. Process families supply RSS
and CPU observations; successful owner history is preferred, then the same gate
kind, with failed peaks retained as a conservative lower bound. Unknown work
reserves 8 GiB and eight CPU cores. Live raw shared-lock holders outside the
queue are sampled by PID and process tree at each admission pass, with nested
trees counted once. Their total CPU, peak-RSS and slot charges are capped at
half the host capacity; RSS already included in `MemAvailable` is credited.
Unattributed or orphaned governed leases retain the conservative RAM reservation.
Measured host CPU replaces I/O-inflated load average for admission, including
after the final external holder exits. The first sample retains the load guard until CPU
counters advance. Waiting logs record the measured footprint, charge and cap.
The launcher requires one explicit placement: `--nvme`, `--ram`, `--exclusive`,
or `--light`. Omitting placement refuses before enqueueing or draining any work.
Command waits observe exit without reaping the leader until its process group
has stopped. The retained child reserves its PID through cleanup, so termination
cannot be redirected to another job by numeric PID reuse.
Status reports fresh resource checks per queued ticket beside its FIFO position and
placement; a recorded ceiling note describes configuration, not a live disk refusal.
After a joined child, a metadata-lock timeout is reported as deferred queue
finalization without replacing the child exit. The next queue scan reaps the dead
wrapper; RAM cleanup errors still fail and preserve their reservation.
Cargo is capped at three jobs per wrapper. Fresh PSI samples stop new admissions
at CPU `some` 25%, memory `full` 1%, or I/O `full` 15% over ten seconds. A
missing or unreadable required pressure sample fails admission explicitly.
The I/O limit is provisional; sparse host history does not establish gate reliability
at a particular pressure value. Memory forecasts and RAM reservations remain required.
Projected peer CPU and measured host CPU, each plus the candidate, must fit
affinity-visible cores minus a margin (one twelfth, at least one core). NVMe keeps
the 60 GB free-space floor. Root retains that floor for unknown or root-resident
placements; verified NVMe targets and governed RAM reservations do not wait for
unrelated root capacity. A failed RAM reservation retains the disk placement guards.
Disk maintenance runs independently of ordinary build admission.
Use `scripts/gate.sh full --exclusive-full` when full tests must run serially,
including inside a scheduler job with verified NVMe or RAM placement. This takes
the full-workspace lock exclusively without making the heavy placement exclusive.
Pass `-FullGate` to `scripts/land.ps1` when a landing requires full qualification
beyond the path policy. This is independent of the `-Tier` review classification.
After five minutes of a disk-floor stall, the queue writes a persistent warning
and prints it loudly in the waiting job's log. `heavy status` repeats a current
warning; recovery clears it. Repeated log warnings are limited to one per half hour.

`heavy status` names the current safety ceiling and any backoff reason. The
bounded resource history lives beside the unchanged ticket/run files in
`~/.local/state/secode/heavy/hardware-runs.json`. Slow test execution does not
lower the concurrency ceiling; memory forecasts and measured pressure govern
new admissions. Current controllers keep their ceiling in `hardware-ceiling-v2.json`,
so an older controller finishing with timing-based backoff cannot lower it.
Older waiters still respect their own ceiling. Queue and full-lock waiting are
excluded from execution times.
An expired historical four-slot observer does not pin current clients to three;
new wrappers sample during execution and record completion without a separate
observer process. The old observer remains a legacy-client diagnostic.
Inherited lease descriptors still corroborate kernel counts, so unrelated lock
churn cannot hide an orphaned child.

Heavy jobs and direct gates start an independent user service before taking
their queue ticket. Closing or relaunching a seat disconnects its output
watcher; the same worker, ticket and queue age continue under the heavy lock.
Light diagnostics and commands already holding an inherited lease stay in
their current supervisor.

```sh
~/.local/lib/secode-release/heavy --ram --owner "$STATEENGINE_AGENT_ID" -- bash scripts/gate.sh auto \
  --base <current-main> --log /tmp/affected.log
```

The launcher prints a job identity and an output log under
`~/.local/state/secode/jobs/<job-id>`. After reconnecting, use
`python3 ~/.local/lib/secode-release/durable_jobs.py --follow <job-id>` to
read the log and return the actual exit status, or `--cancel <job-id>` to
stop and join that job. Its terminal `result.json` names the owner, source
commit, log and exit status; it is a job result, not a qualification receipt.
Missing results fail visibly. Each log is bounded to 64 MiB, services to one
day, and retained history to 256 jobs; archive completed records when full.
Existing jobs started by older runners keep their original lifecycle.

For a small offline diagnostic that does not invoke Cargo or a gate, use:

```sh
~/.local/lib/secode-release/heavy --light -- python3 /path/to/offline_measurement.py
```

Light work bypasses the build queue at idle CPU/I/O priority, with one job at a
time, a five-minute wall limit, and per-process limits of 1 GiB address space and
300 CPU seconds. A busy light slot refuses immediately without creating a build
ticket. Compile and test commands must use ordinary admission.

The full gate defaults to three hours (10,800 seconds), both when invoked
directly and through the landing helper. `--timeout` overrides the direct
gate's execution limit; queue waiting has its own separate limit.
Landing compositions selected as affected use the reviewed-mode allowance of
5,400 seconds. Gate execution timeouts exit 124; the landing helper reports
"timed out at N s". Command and test failures retain their failure diagnostics.
Disk gates also isolate `TMPDIR`, `TMP` and `TEMP` in a private directory.
The supervisor removes disposable test fixtures after success, failure or cancellation;
keep qualification logs outside this scratch directory.
New lane worktrees belong under `/home/example/grapt/worktrees/`; the landing
helper also accepts existing worktrees under the user's home directory.
Create a producer worktree from a named accepted commit with
`bash scripts/build/create-worktree.sh NAME FULL_ACCEPTED_COMMIT`. It creates
the branch and installs local build defaults. Landing composition checkouts and
the nightly clone also default to this NVMe parent.
To relocate an idle lane across filesystems, stop its jobs, ensure the destination
does not exist, move its directory with `mv <old-path> <new-path>`, then run
`git -C <main-checkout> worktree repair <new-path>`. From the new path, run
`bash scripts/build/configure-worktree.sh --refresh` to rebind its generated Cargo
defaults. `git worktree move` works only within one filesystem. Customized configs
are preserved and require manual updates.

Install the shared box launcher from a reviewed checkout without fetching or
changing timers:

```sh
control="$HOME/.local/lib/secode-release-snapshots/$(git rev-parse HEAD)"
python3 scripts/build/install_schedules.py --launcher-only --control-directory "$control"
"$control/heavy" status
"$control/heavy" history
```

Every heavy job uses this launcher in place of raw `flock`. Tickets contain the
owner seat (`STATEENGINE_AGENT_ID`, or explicit `--owner`), priority class,
monotonic enqueue time, PID and boot/start identity. Status prints every current
holder and the ordered queue with owners. Tickets are ordered by class, then
enqueue time: a landing that holds the canonical lease keeps qualification and
composition in one class-0 reservation. Standalone gates, including full
qualification, remain class 1. The hygiene timer uses its own maintenance lock and
target leases, outside the build queue. Legacy `--exclusive --hygiene --owner disk-hygiene`
callers always use class 0; an older waiting hygiene ticket is promoted too.
Do not use `--class 0` for standalone builds or gates. Landing priority does not
preempt an active job or bypass hardware admission and execution bounds.

RAM gates retain their independent 24 GiB per-run and 72 GiB aggregate
reservation ceilings, the 24 GiB allocator floor, and cgroup memory/swap/kill
controls. Resource admission cannot waive these checks. A failed RAM reservation
can share a verified NVMe fallback; other disk fallback first joins its RAM scope
and then obtains exclusive admission.

`--ram` requires a governed `bash scripts/gate.sh` command from a compatible
checkout. Use `--nvme` for other commands with verified NVMe targets. Their nested
gates retain disk placement; use `heavy.slot(ram=True, nvme=...)` when a compound
driver needs scheduler-reserved RAM with an NVMe fallback. Older wrappers that
already started nested RAM gates retain the full 24 GiB growth charge without
unverified RSS credit. Unmarked commands
remain exclusive, including their nested gates, so a
shell can safely continue after a nested gate fails or is cancelled.
Cargo output and test scratch (`TMPDIR`, `TMP`, `TEMP`) live in
the same throwaway tmpfs scope. Heavy jobs and gates append `/tmp` and
`/home/example/grapt/scratch` to `GIT_CEILING_DIRECTORIES`, preserving existing
ceilings, so a stray repository at a shared scratch root cannot capture fixtures.
Filesystem-based location and mention discovery honor these ceilings too.
Status shows each holder's mode, reservation,
hold time and whether it overlapped another holder. History keeps the last 128
jobs and prints the latest 16, including target modes and exit status.

The queue is shared across checkouts in `~/.local/state/secode/heavy`, with at
most 1024 jobs. Dead or reused PIDs are reaped; malformed state refuses new work.
Notification sockets disappear on exit. A one-second fallback detects crashed
owners and releases from legacy jobs. The actual critical section still takes
`/home/example/secode-heavy.lock`, shared for governed RAM/verified NVMe and exclusive for other disk work,
so legacy callers cannot overlap wrapped work;
legacy callers cannot honor ticket order until they migrate. Cancellation joins
the command group before releasing its slot. The command inherits the lock so a
killed wrapper cannot unlock a still-running child. Existing queued clients keep
their v1 ticket format and single compatibility holder flag; a separate bounded
resource file records all actual holders under the same metadata mutex.

A lock timeout exits 75 and names the waiting stage, queue position and holder,
for example `heavy lock not acquired after 10800 s, position 2 behind agt-... pid
1234`. This is a queue timeout; composition has not started. The landing helper
streams this diagnostic and reports the queue timeout separately.

The scheduler/build helper suite runs only for changes under `scripts/build/`
or to `scripts/land.ps1`, `scripts/gate.sh` or `scripts/build-cache.sh`.
Unrelated TUI/API/Rust changes keep their scoped Rust and law checks. The nightly
full gate includes the helper suite even when none of those paths changed.

The affected gate checks formatting on touched crates, strict clippy and tests on
those crates and their direct dependents. API changes also include the TUI. Every
slice runs `no_god_files`, `direct_include_format` and `write_chokepoint`; crate
selections already running these tests do not repeat them.

Rust changes also compile the affected crates' Windows tests with `cargo xwin
test --no-run` and lint all their Windows targets with `clippy -D warnings`.
These checks cover ordinary features as well as the Linux tests' fixture
features, and never execute Windows binaries. Cross artifacts reuse the checkout's persistent NVMe target
and normal cache admission, even inside a RAM-backed Linux gate. Documentation
and build-only slices add no unaffected Windows builds; nightly covers the full
Windows workspace.

Linux affected and full gates run ordinary test binaries with nextest, three
workers and no retries. Install the measured version before running these gates:

```bash
cargo install cargo-nextest --version 0.9.143 --locked
```

The gate keeps the same package and feature selection, skips only the exact
nightly test names, and leaves ignored tests ignored. Cargo runs doctests
separately with the same scope. Nextest retains JUnit beside the gate log as
`<log>.nextest.xml`, including skipped cases and failed output. The receipt
requires that report to remain unchanged and green. Tests taking about a minute
or longer are named in the log for nightly classification.

Targeted law and ordinary-feature checks retain their Cargo runner. When Cargo
reports named test failures in a binary outside the affected crate set, the gate
retries only those tests once, separately, with one test thread.
Cargo artifact metadata identifies the owning crate and target. A failure in an
affected crate, a build error, an interrupted process or an incomplete failure
summary stays red. A retry must use the original test executable; a changed
feature selection that rebuilds another binary cannot qualify. At most 16 failed
tests may be retried per gate.

If every isolated retry passes, the gate exits zero with `PASS_WITH_RETRY`.
Its receipt retains the original failure set, the original output and each
retry's command and result. The log includes Cargo artifact diagnostics as well
as test output. The landing verifier recomputes the affected set from the exact
source and base, requires every retry to be outside it, and checks the original
and retry log spans. Changing the log, retrying an affected test or failing a
retry refuses landing.
A retry log cannot be relabeled as plain `PASS`, and every retry marker must
match exactly one recorded attempt. Logs over 64 MiB are refused before reading.

Ask one reviewer to read the slice. After ACCEPT, rebase on current `main`, rerun
the affected gate and land it yourself from the Windows controller:

```powershell
./scripts/land.ps1 -Branch my-slice -AcceptedCommit <rebased-commit> -Tier 2 `
  -LinuxRoot /home/example/rust-my-slice -Owner $env:STATEENGINE_AGENT_ID -GateLog /tmp/affected.log
```

A reusable receipt must cover the exact clean commit and current `main` base,
with every required command and an unchanged log. If `main` advanced on disjoint
paths, the helper rebases the reviewed patch under the heavy lock, checks all
workspace tests compile and runs strict clippy on the affected crates and their
direct dependents. A separate composition receipt records the original gate,
both bases and the new commit; it does not relabel the original test result.
Overlapping source paths, a changed patch or a failed check stop the landing.
An overlap confined to the generated `licenses/dependencies/NOTICE.html`
instead regenerates that register on the composed tree and requires the license
check to pass before the composition gate. The composition proof binds the
generated bytes and license log. Upstream-adaptation notices are
separate `notices/` fragments, so they do not overlap; the generated, untracked
`THIRD_PARTY_NOTICES.md` is assembled by `scripts/release/notices.py` and shipped
by packaging. A candidate based on a tree that still tracked that file may overlap
when Git can merge its edits without conflicting hunks. Each
version must be a regular UTF-8 text file no larger than 1 MiB; additions,
deletions and mode changes refuse. The helper checks the merged bytes and runs
fresh affected qualification (full qualification for a gate-cost exception) before landing,
with its own verified receipt. Conflicts name the document and refuse
before rebasing. Other authored documents, code and license inputs keep the
ordinary overlap refusal.
Composition checks use the same bounded RAM target, disk fallback and joined
cleanup as other gates. Each attempt exclusively reserves its log; only its
validated RAM child may reuse that empty file once. Stale RAM hints and nested
attempts cannot overwrite existing evidence. Log and RAM measurement survive target cleanup;
the composition proof binds the complete result. A human notice overlap also
requires the new affected/full receipt; it cannot reuse compile-only coverage.
Unrelated dirty files and their index entries on the controller are preserved.
A final canonical change during these quick checks refuses safely; retry from
the preserved candidate. The canonical SSH alias defaults to `windows` and can
be set with `-ControllerHost`.

For the reviewed R input composition, retain the original full result explicitly.
First compose onto current `main` and obtain ACCEPT for that exact clean commit.
This exception supports Tier 2 landings only. A Tier 1 landing requires a fresh
full gate on the composed commit: the retained full predates that tree, and an
affected run or test-graph compile does not establish full workspace execution
on it. Passing retained-full arguments to the receipt verifier cannot promote
an ordinary affected receipt to a reviewed or full result.
The only permitted changes to the original authored patch are
`crates/grapt-client/src/client.rs`, the R `main.rs`, `record_input.rs` and
`windows_input.rs`, and `notices/` fragments. These input-adapter changes
must remain Tier 2; authority or credential changes require their normal review
and qualification. Every other authored patch entry, including dependency, core,
build and security work, must be identical to the original qualified patch.
Canonical-only changes are compared against their own accepted base. When
canonical edits overlap an already-authored Product test, the helper recomputes
the exact three-way text merge of the qualified change onto that accepted base.
Only identical merged bytes qualify; conflicts, owner changes and file-type
changes refuse. Such tests join the fresh affected scope. Production, core,
dependency and build paths still require identical raw authored patch entries.

Run the explicit gate under the ordinary class-1 scheduler:

```sh
bash scripts/gate.sh reviewed --base <reviewed-current-main> \
  --retained-commit <original-full-source> --retained-base <original-full-base> \
  --retained-log /absolute/original-full.log --log /absolute/fresh-reviewed.log
```

This mode verifies the original full receipt and runs a workspace test-graph
compile plus normal affected formatting, lint, tests, licenses and law checks
for the fresh input changes. Its receipt says `reviewed`, never `full` or
`affected`. Original logs and receipts are preserved, and changed, stale or
incomplete retained results refuse. The gate requires clean source and reserves
a new log before execution. After ACCEPT and successful qualification, land that
same reviewed commit:

```powershell
./scripts/land.ps1 -Branch my-composed-r -AcceptedCommit <reviewed-commit> -Tier 2 `
  -LinuxRoot /home/example/grapt/worktrees/my-composed-r -GateLog /absolute/fresh-reviewed.log `
  -ReviewedBase <reviewed-current-main> -RetainedFullCommit <original-full-source> `
  -RetainedFullBase <original-full-base> -RetainedFullLog /absolute/original-full.log
```

All retained inputs are required together. This path keeps the controller's dirty
preservation and final ancestry checks. It refuses an automatic rebase or a
canonical advance after review: compose again and obtain a new exact-commit
review. Ordinary overlapping source still refuses without this explicit path;
ordinary affected and full receipt verification cannot borrow a reviewed result.

Gate cost follows the 2026-10-24 path policy. Use `scripts/gate.sh auto`
to select it from the diff against `--base`. Full qualification is required for
changes to the five core crates (`grapt-types`, `grapt-store`, `grapt-runtime`,
`grapt-event`, `grapt-schema`), build/gate tooling, and release packaging.
Workspace build-policy changes also require full qualification; additive workspace
members/dependencies and equivalent lockfile edge disambiguation retain the changed
crates' scope. Actual resolution changes keep broad coverage. Row A package
builds use `scripts/gate.sh auto --row-a` to require full qualification even
when the package source has no diff.

All other slices use the affected gate. `-Tier 1`/`-Tier 2` retain the review
classification; the landing controller independently selects the required gate
from both sides of the reviewed diff. Affected receipts for an exception are
refused, including receipts from older checkouts. Tier 1 still requires its
security/failure review. `-SkipFullIfGreen` remains an alias for `-GateLog`.

Tests over about 60 seconds are tagged by exact name in
`scripts/build/slow_tests.py`; landing qualification skips those names, while
nightly full qualification runs them at their real workload. Missing or renamed
tagged sources fail the gate. Serial test output records `nightly-slow-test`
lines for newly observed slow tests so their owners can add a precise tag.
Normal full qualification still compiles and lints every test target.
Native Windows and macOS checks run only for platform-specific changes.

Canonical Windows landings join a FIFO queue before running Git. The controller waits
up to 45 minutes by default; `-LeaseWaitSeconds` changes that limit (zero tries once).
While waiting, it prints its position among waiting callers every minute and
identifies the current holder by branch, accepted commit, start time and PID.
A timeout retires only that caller;
the exclusive file handle releases automatically when its holder exits or crashes.
Older controllers may hold the lease without supplying owner metadata.

## Completed build jobs

The durable build-job index (`~/.local/state/secode/jobs/`, at most 256
slots) is shared by all seats. Two entry points free slots, and the
installer deploys and enables both:

- The five-minute `secode-job-history.timer` runs the controller's
  `durable_archive.py --apply --agent-id <controller seat>` against the
  live index. It moves the oldest eligible stopped terminal jobs into
  `~/.local/state/secode/completed-jobs/`, retaining every original file,
  at most 32 jobs per pass.
- When an admission finds the index full, `durable_jobs.py` drains under
  its emergency rule (finished records older than two hours move to
  `jobs-archive/` and the stored log paths are repointed to the new
  location). The drain keeps that flat two-hour rule and does not
  consult the receipt pin or the lifecycle lock; the scheduled sweep and
  the manual archive enforce the full policy.

The timer's retention policy keeps current proofs: successful records are
eligible once both terminal timestamps (finish time and record mtime) are
at least two hours old, failed records at least six hours old, or failed
records at least one hour old whose recorded checkout has disappeared or
moved to another commit. Any terminal record older than six hours is
eligible regardless of exit status. Active services, held lifecycle
locks, pinned records, recent results, unreadable records, and
current-source proofs are preserved. A record whose terminal result does
not parse is preserved indefinitely and keeps counting toward the 256
slot cap; inspect it manually.

Create `retain-for-receipt` inside a job directory to preserve a pending
receipt beyond those windows; the scheduled sweep and the manual
archive refuse that marker, but the admission drain does not consult
it, so a pinned finished job still moves to `jobs-archive/` when the
index fills before it drains. A receipt may remain valid for 24 hours,
so pin the job before its two-hour window if a pending landing still
references the original job path.

To archive one explicitly selected completed job, for example to free a
slot the policy still holds:

```sh
python3 ~/.local/lib/secode-release/heavy archive DURABLE_JOB_ID
```

Use the durable job token printed by the heavy runner, and select records
no longer referenced by a gate awaiting landing; archival changes the
record's absolute location. The command refuses live services, held
lifecycle locks, and terminal results that are missing, not an object,
unconfirmed, or less than one minute old. `durable_jobs.py --follow JOB_ID` resolves a job in the
live index or either archive root and keeps reading if the job moves
between roots while being followed. A destination collision preserves
both records. Run `durable_archive.py` without `--apply` to preview a
sweep without changing records.

## Nightly full gate

After the scripts have landed, install the user timers on the workstation:

```sh
control="$HOME/.local/lib/secode-release-snapshots/$(git rev-parse HEAD)"
nohup python3 scripts/build/install_schedules.py --control-directory "$control" \
  --agent-id "$STATEENGINE_AGENT_ID" \
  > /tmp/grapt-schedule-install.log 2>&1 < /dev/null &
# After the installer finishes:
systemctl --user list-timers 'grapt-*'
```

Pass `--last-green <commit>` on first installation when a known full-green
ancestor is available. It is rechecked before attributing a failure.

The installer creates a dedicated `/home/example/grapt/worktrees/nightly` clone of canonical Windows
`main` and installs its supervisor in the fresh versioned controller directory; it refuses to take over an existing unmarked checkout. At 02:00 workstation
time, the nightly service fetches `main` and runs `scripts/gate.sh full --nightly` under the
heavy lock, including all nightly-only slow tests. Results and logs are in `/home/example/grapt/logs/nightly` and the user
journal. Owned logs retain at most 16 attempts and 512 MiB, preserving the current
attempt and `last-result`; oversized failed output retains a visibly marked tail.
Each full attempt has three hours, including the slow tier; diagnosis has a
twelve-hour budget and refuses to begin an attempt that cannot fit. The service
allows queue waiting separately, with a 23-hour outer bound. Existing queued
nightlies finish under their original admission; timer updates do not restart them.
Unrelated files are preserved.

```sh
cat /home/example/grapt/logs/nightly/last-result
journalctl --user -u secode-nightly.service
```

A red run rechecks the last green source and repeats the candidate failure before
bisecting at most 256 landed
commits along first-parent history. It names the first failing commit and its author, who fixes it first thing, rather than
assuming the latest landing caused the failure. Before naming a culprit, it
checks that reverting it in the dedicated clone restores a green full gate.
A failing baseline, conflicting revert or unrelated regression reports an
inconclusive result; no canonical ref changes during diagnosis. The clone returns to the tested `main`
commit after diagnosis. The culprit's owner fixes it first thing and qualifies the correction through
the same path-based policy; a revert uses the landing helper too.

## Automatic disk hygiene

The whole-cache reclamation target is 300 GiB, configured by `CAP_BYTES` in
`scripts/build/evict_targets.py`. Above the target, eviction and generation
pruning reclaim eligible output in least-recently-used order. Protected and
unknown outputs remain intact. Builds continue above the target while the
resolved filesystem satisfies its 60 GB free-space floor and build reserve;
protected output above 300 GiB must not deadlock the queue. The five-minute
capacity check requests cleanup above the target as well as below the free-space
warning threshold. Scheduled cleanup records measured totals, remaining excess
and failures under `/home/example/grapt/diagnostics` on the seven-day retention path.
The maintenance timer removes build
targets idle for an hour, or marked finished by a successful landing. Landing
also requests an immediate maintenance pass; live references still protect its
outputs. A new build clears the finished marker. Managed shared-cache targets,
retired toolchain layouts, standard cross-compilation outputs and checkout-local Cargo targets are eligible; source checkouts,
Git history, compiler cache, shared frozen fixtures and unknown content are preserved.

Qualification targets require a retained owner marker or a recognizable Grapt
checkout. Unattributed and foreign qualification outputs are listed and preserved.
A missing checkout does not pin its attributable outputs forever. Idle targets
with recent output retain their current builds, but older duplicate executable
hashes and matching dependency files are pruned, keeping the newest per test name
and compilation profile. Libraries, unknown files and linked paths are preserved.
Existing owner checkouts remain eligible for LRU eviction under budget pressure
when their processes, open files, queued jobs and build leases are idle. Standard
Windows target directories and checkpoint-test scratch count as derived output.

A live seat may retain its target while superseded artifacts expire between builds.
Pruning takes both the target lease and Cargo profile locks, and rechecks build,
queue and open-file references. It keeps the newest test executable and two
incremental configurations per crate and profile, plus recently modified output.

Live-process, open-file attribution and build leases protect active targets. Old gate
retention tags do not keep idle compiled output forever. Cleanup has its own
15-minute maintenance window instead of consuming a build admission's one-minute
budget. A cleanup failure is reported as a failure, without starting Cargo.
Foreign builds require a complete observation of Cargo and all descendants:
working directories, open descriptor targets, arguments, environment and argument
response files. Any managed-target reference or unreadable observation blocks
cleanup. Cargo output resolution supplies additional reasons to block, including
managed incremental output, unresolved aliases and unsupported configuration;
it never substitutes for the process-tree observation. Attribution does not build
or change the foreign project; workspace discovery uses `cargo locate-project`.

The separate five-minute disk-pressure timer samples the cache and root
filesystems. Below 100 GB on the cache filesystem it lowers both ordinary
admission ceilings to at most six, preserving stricter operator limits and the
reserved landing slot. Notifications require either filesystem below 80 GiB, or
a change greater than 5 GiB since the last alert on a filesystem currently
below 100 GiB. Changes on a filesystem with at least 100 GiB free do not trigger
a notification. An old alert record without size samples establishes a quiet
baseline on its first observation. Successful delivery advances that baseline; failed
delivery retains it for retry. Successful alerts repeat at most every 30
minutes; failed delivery retries after five minutes. The 60 GB admission floor
is unchanged. Running work continues; restore capacity explicitly only after
reclamation and a stable free-space trend.
Inventory reports both the cache filesystem and `/`, so an NVMe cache bind mount
is measured separately from SATA root usage.

On the PC, daily retention runs at 05:00 PC local time. It expires only
completed UUID runs older than seven days beneath `<native-gate-root>/runs`,
using `<native-gate-root>/.retention.lock` and its crash fence. Logs are
`<native-gate-root>/retention.log` and `retention.previous.log`. It never
walks the drive, scans or cleans `E:/temp`, or removes source worktrees. Git
markers and bare repositories preserve the whole candidate run. Old
native runs beneath `E:/temp` remain untouched. New native workers retain the
legacy operator lease as well as the retention lease until child cleanup joins.

Preview or run maintenance explicitly; it takes its own locks:

```sh
python3 ~/.local/lib/secode-release/disk_hygiene.py --dry-run
python3 ~/.local/lib/secode-release/disk_hygiene.py
journalctl --user -u secode-disk-hygiene.service
```


Exclusive work uses the heavy queue's bounded drain. After five minutes, a
waiting exclusive job stops new class-1 shared admissions once existing jobs
can drain within 45 projected minutes. Class-0 shared work can still enter.
During the first hour, jobs with longer or unknown projections finish before
the drain starts.
Every new launcher ticket declares an exclusive ceiling (default 1,800 seconds;
`--max-hold` may lower it) and a shared duration projection
(`--projected-seconds`). When omitted, the scheduler uses the median of recent successful
holds for that owner, then that gate kind, then full or other measured gates. A new queue without
measurements uses a 45-minute bootstrap estimate. The execution ceiling starts
when the exclusive lease is acquired, excluding queue wait and earlier shared work.
A timeout stops the owned process group and exits 124. Retry with the same
`--owner` and `--refund <ticket>` printed by the timeout to retain the original
queue age and priority; credit is consumed once, without automatically rerunning work.

Drain start/end events and sampled idle-slot minutes are in
`~/.local/state/secode/heavy/exclusive-drain.jsonl` (one bounded rotated file).
Already-running launcher versions retain their existing policy. An owner must
replace an old waiting exclusive launcher with the updated helper before it can
use a bounded drain; do not terminate another seat's waiter.

A drain lasts at most 45 minutes, including when class-0 work keeps arriving.
On expiry or after an exclusive hold ends, a five-minute window gives waiting
shared jobs precedence over another class-1 exclusive. Class-0 priority remains.
Gate and landing subprocesses stop and join their owned groups before a lease
expiry can return the lock or issue retry credit.

An exclusive that has waited 60 minutes starts a forced drain regardless of holder
projections or earlier deferrals. New class-1 shared work waits; class-0 work can
still pass and running jobs finish under their existing caps. Waiting shared jobs
retain their five-minute progress window between forced drains. Forced drains are logged
as `forced-drain` events with the ticket, owner and measured queue wait.

### Replace your own waiting launcher after a scheduler update

Already-launched Python clients retain their loaded scheduler. After the reviewed
replacement is installed, each owner can retire only its own still-waiting client:

1. Preserve the original background-job request, command, environment/profile,
   checkout, candidate/base and qualification arguments. Confirm that this is your
   job, and capture its exact ticket, owner, PID and boot/start identity from the
   queue. Do not replace an admitted job, a conversion waiter or unrelated nightly.
2. Run the installed helper with those exact values:
   `python3 ~/.local/lib/secode-release/replace_waiter.py --ticket TICKET --owner OWNER --pid PID --start START`.
   Under the admission mutex it rechecks identity and refuses any client already
   admitted. It binds cancellation to the process with a pidfd and gives the old
   launcher 15 seconds to exit normally; it never signals a process group.
3. Confirm that the old background job has exited. Start its preserved work once
   through the installed `heavy` launcher, adding the printed `--refund TICKET`
   and retaining `--owner OWNER`. Retry credit preserves class and original queue
   age, is single-use, and cannot be redeemed while the old ticket is live.

On refusal or an exit timeout, inspect the original job and stop the replacement
path; never launch a duplicate or edit queue files. Running jobs finish with their
existing policy. Waiter replacement is logged separately from a hold timeout and
does not count as a completed execution or a duration measurement.

A queued landing controller can refresh its scheduler without discarding queue
age: retire only its proved unadmitted Python waiter with `replace_waiter.py`,
wait for the Windows controller to exit and release its landing lease, then
rerun the same reviewed controller and arguments with `-RefundTicket TICKET`.
The compose admission redeems that owner-bound credit once. Preserve running
or conversion holders; do not wrap the controller in another heavy lease.
