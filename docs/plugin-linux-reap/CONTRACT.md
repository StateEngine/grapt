# PLUGIN-LINUX-REAP-01: contract

- Charter: `docs/campaigns/PLUGIN-LINUX-REAP-01.md` (blob `da95368c`, byte-verified from the controller).
- Base: dev `3ac2fb5ec0f05a3bea845214ee3c348ba1ccdfde`, branch `plugin-linux-reap`.
- Admitted with scope additions (RSS case in J01), a memory-side ruling and a W2 seam ruling.

## Process model the contract is about

The rust-side child the supervisor spawns for a plugin bridge is the
`systemd-run` LAUNCHER, not the node process:

    systemd-run --user --quiet --wait --pipe --service-type=exec \
        --unit=secode-plugin-<pid>-<gen>-<n> \
        --property=MemoryMax=<threshold + headroom> MemorySwapMax=0 \
        OOMPolicy=continue SuccessExitStatus=SIGKILL RemainAfterExit=yes \
        KillMode=control-group SendSIGKILL=yes \
        --slice=secode_plugins_<pid>.slice \
        /usr/bin/env -i ... <node> <NODE_ARGUMENTS> <bridge>

Node is the unit's MainPID, a child of the user manager — not of the host
process. `stop_unit` therefore cannot exit the launcher by itself: it
SIGKILLs the unit's cgroup (`systemctl --user kill --kill-whom=all
--signal=KILL <unit>`, which kills node) and then files a fire-and-forget stop
job (`systemctl --user stop --no-block <unit>`). The launcher's own exit waits
on the user manager to COMPLETE that stop job — a D-Bus round trip through the
shared manager, which under parallel carrier load routinely outlives a
microsecond-scale poll. This is the J01 mechanism.

## Termination contract (linux)

Termination is a two-phase bounded sequence:

1. **Grace** (`cleanup_millis`, default 5000 ms). Stop containment, then poll
   the launcher for a normal exit.
2. **Reap** (`reap_millis`, default 5000 ms, new `BridgeLimits` field). If the
   launcher is still alive at the grace deadline, kill it, then poll for the
   reap until the total bound. The bound is checked before every poll, so a
   reap racing the bound cannot mask the distinct outcome.

**Total bound = cleanup_millis + reap_millis — 10,000 ms at the defaults,
stated as a single number.** Every termination path must return — Ok,
SurvivesKill, or Protocol — within the total bound; nothing on this path may
wait beyond it. The W2 witness states its own instance of the same bound as
a single number (cleanup 100 ms + reap 300 ms = **400 ms**) and asserts
elapsed >= 350 ms (the post-kill bound ran to completion before the outcome -
a single immediate poll cannot reach it) and <= 600 ms (the bound plus 200 ms
poll-granularity slack).

The three outcomes stay distinct, per the charter:

- **Reaped** — the launcher exited within the total bound: `Ok(())`.
- **SurvivesKill** — the launcher was killed but not reaped within the reap
  bound: `Err(PluginError::SurvivesKill)`, message
  `plugin bridge survived kill: plugin bridge launcher for containment unit
  <unit> was killed but not reaped within the post-kill bound of <reap> ms`.
  A child that cannot run its exit (unkillable kernel state, wedged manager)
  lands here, bounded, and the supervisor is not hung.
- **Protocol** — the child's status is unreadable (custody lost, wait error):
  the existing fail-closed outcome.

A grace deadline with a live child no longer reads as a timeout on linux: the
child is killed and given the reap bound. The base's single immediate
`try_wait` after `kill()` — the `Timeout("process cleanup")` red — is removed.

### The W2 seam witness

The survives-kill outcome is witnessed at the `post_kill_reap` seam in
`src/supervisor/process.rs` - the private free function that owns the
post-kill reap loop, whose child-status probe is an injected
`FnMut() -> std::io::Result<bool>` (`Ok(true)` = exit reported,
`Ok(false)` = still running, `Err` = status unreadable, custody loss
included). The production path wraps the real `Child::try_wait` in
`reap_after_kill`; the witnesses inject deterministic doubles. The clock
stays real and bounded (`tokio::time::Instant`), so the witness asserts a
real elapsed bound rather than a counted iteration. The `#[cfg(test)]`
module of `process.rs` carries three witnesses (lib target;
cross-platform by the ruling's design because no kernel, containment, or
carrier feature is in scope):

- **the PLR101/PLR102 witness** - a probe that never reports an exit must
  run the full bound and land in `Err(SurvivesKill)` naming the
  containment unit; asserted elapsed >= 350 ms (the post-kill bound ran to
  completion before the outcome - the single-immediate-poll shape cannot
  reach it) and <= 600 ms (the 400 ms bound plus 200 ms
  poll-granularity slack); the probe runs on a thread joined under a 10 s
  bound, so an unbounded drain (PLR102) turns the witness red instead of
  hanging the harness;
- **the reap-before-bound witness** - a probe that reports its exit on the
  third poll returns `Ok` before the bound: the distinct outcome exists
  only at the bound;
- **the unreadable-status witness** - a probe that errors stays
  `Err(Protocol)` with the read error preserved: neither a reap nor a
  survives-kill.

Why the seam (the end-to-end state is unreachable on a healthy kernel):

- The real launcher (`systemd-run --wait`) does no file I/O in its wait
  loop - it waits on the user manager - so it never enters an
  uninterruptible kernel state, and a SIGKILL always reaps it. A node
  stuck in D state does not shield the separate launcher process, which
  the kill reaches directly.
- The only SIGKILL-surviving state is a true D-state wait, which no
  launcher path produces. The superseded frozen-cgroup design is refuted
  by measurement: a frozen task killed with SIGKILL becomes a zombie in
  milliseconds, which the product's own `try_wait` reaps inside the grace.
- A witness that manufactures a D-state child (the C FUSE daemon design)
  was dropped by the W2 seam ruling: a
  test-time-compiled C daemon depending on `cc`, `fusermount3` and kernel
  FUSE protocol details is fragile, environment-bound machinery for one
  witness, and would red on any carrier without FUSE. The D-state
  analysis stands as the justification for witnessing the bound at the
  seam instead.

W1 (integration level, `tests/bridge/reap.rs`) remains the positive
witness for the whole Defect-1 mechanism end to end: a SIGTERM-ignoring
bridge shuts down inside cleanup + reap and the supervisor recovers a
fresh generation. Honest scope (review P3): the node fixture ignores
SIGTERM, but the launcher (the systemd-run wrapper) usually exits inside
the cleanup grace, so the post-kill path is not forced by W1; W2 is the
deterministic kill-path witness.


## Attach contract (linux)

The watchdog's cgroup attach locates the unit's ControlGroup before
telemetry starts. The attach is bounded by the generation's own
`startup_millis`, and the bound is enforced per query:

1. Every ControlGroup query runs as a bounded subprocess (`systemctl --user
   show --property=ControlGroup --value`): it is polled on the bounded
   clock, killed and reaped if it outlives the REMAINING budget, and reads
   at most 4097 bytes, so a stalled D-Bus query cannot hold the attach past
   the bound.
2. A query that overruns its wait is killed, reaped, and reported as its
   own TimedOut outcome; the attach then fails closed at the deadline
   instead of waiting on the stalled query.
3. The poll sleep is clamped to the remaining time, so the final tick
   cannot overshoot the deadline.

**Total attach bound = startup_millis + one 20 ms query-poll tick of
scheduling slack, stated as a single number.** The pre-fix attach had no
per-call bound at all: a fixed 30 x sample attempt count, each query an
unbounded `systemctl .output()`. A healthy generation's cgroup
materialising under D-Bus contention could starve the attempt budget (the
C12 sxs3 residual class: locate_cgroup 30-sample budget, 1x sxs2 -> 29x
sxs3, load-scaled, rotating victims), and a stalled query could hold the
attach past the startup bound forever. Both resurrection shapes are
witnessed dead (PLR108: the stalled-query seam reds on the elapsed bound;
the delayed-cgroup witness stands in-tree green as the end-to-end proof,
unit activating at 6 s inside the 10 s startup bound).


## Telemetry-loss contract (linux)

When a watchdog sample cannot read the unit cgroup telemetry (the cgroup is
pruned while the launcher is still exiting, or the files are unreadable),
the missing branch resolves in a fixed order:

1. **A dead child ends the watchdog silently** (the supervisor already
   observed the launcher's exit): the stream-end path owns the
   classification, so the watchdog records nothing for it.
2. **The OOM verdict is evaluated first**: the containment query
   (`systemctl --user show --property=OOMKills,Result,LoadState`)
   classifies systemd's own evidence with the R2 precedence (an explicit
   `Result=oom-kill` wins under any LoadState; PLR109). A positive verdict
   terminates the bridge as a Resource violation; an unreadable, failed, or
   unclassifiable query fails closed with the OOM-unavailable outcome - an
   OOM-killed main is a violation, never a silent exit.
3. **A settled terminal main state defers to the stream end** (PLR-R8):
   the main-state query reads `LoadState,SubState`, and only `loaded` or
   `not-found` beside `exited`, `failed` or `dead` defers. The launcher's
   exit lags the unit's state (shape probe: the unit settled 100-290 ms
   after the main process left while the launcher was still running at 4 s),
   and in that window the emptied unit cgroup is pruned, so this branch is
   the watchdog's only view of the known exit. Every other value -
   unsettled or unrecognised SubState, unrecognised LoadState, missing
   property, overlong or non-UTF-8 output, a failed query, a transport
   error - defers nothing.
4. **Otherwise the missing counter advances, and the third missing sample
   terminates the bridge fail-closed** with the
   telemetry-became-unavailable outcome.

Witness (QCU r3): the watchdog reads each decision's sample through a
private reader; production passes the actual cgroup reads. PLR112 holds a
real unit's watchdog at that seam, kills the published main process,
awaits the settled state, then injects absence at the sampler boundary:
green ends at the first missing decision, and the deleted-check mutant
fails closed at the third. The witness no longer removes the cgroup.

The attach-failure branch (locate_cgroup returns None) runs the same
decisions without the window: dead child, OOM verdict, settled-exit
deferral, then the immediate fail-close. The settled-exit deferral is
the attach-branch twin of the PLR-R8 decision in item 3, and since
R11 it is a distinct predicate (attach_process_exited_evidence) over
the shared decision: it defers ONLY a settled terminal SubState under
LoadState loaded. R11 (finding
PLR-R11-72933DEA-ATTACH-NOTFOUND-FAILS-OPEN, review RETURN on
80f3c50e): the R10 shared predicate also accepted LoadState not-found,
and a never-created unit reads not-found/dead with rc 0 (verified on
this box: LoadState=not-found, SubState=dead, OOMKills=UINT64_MAX,
Result=success, rc 0) - the not-yet-created state - so the branch
deferred a unit that never started and returned silently; under the
measured funnel (15-25 s activations against the 10 s production
attach bound) a unit activating just after the deadline ran unwatched,
with no RSS or CPU policy. The attach branch never attached, so
not-found there is ambiguous - never started, or gone mid-attach - and
ambiguity fails closed; the sampling branch keeps not-found (item 3),
because its unit was attached and a unit gone there is a settled exit.
The order is unchanged, so a dead child records nothing and the OOM
verdict still wins (an OOM-killed main is a violation, never a silent
exit). A generation that cannot find its cgroup at attach has no
telemetry to wait for, so it does not spend a 3-sample window; the
branch adds at most two bounded queries (the containment verdict and
the main-state read, each at most the 10 s allowance) to the attach's
own startup bound. The branch is witnessed by the PLR115 decision
seam (the attach predicate over the loaded/terminal/not-found/
unsettled/unreadable table) and the PLR116 control witness (never-
created unit name + real live child reaches the branch and records the
telemetry-unavailable fail-close, end-to-end and deterministic); the
R10 end-to-end stopped-unit construction is retired because it
assembled exactly the not-found shape that must now fail closed. The
decision's unsettled input is the PLR113 seam row - an unsettled,
unknown, or unreadable unit fails closed - because an unsettled attach
failure is not physically sustained on this carrier: a running unit
keeps its cgroup, so the attach succeeds. The shutdown race (the unit
stopped or reset under a not-yet-attached watchdog) resolves through
the dead-child check in the ordinary case - the launcher observes the
stop in under 50 ms, so by the attach deadline the child is gone and
the stream-end path classifies the stop; the residual not-found-with-
live-launcher shape is the PLR116 fail-close at attach.

**Worst-case bound, stated as a single number: 3 x (sample + 20 s)** for
the sample-missing window. Each of the three missing samples can spend the
sample sleep, the containment query (<= 10 s - the query itself is the
bounded subprocess, polled, killed and reaped at the 10 s allowance, the
same stack and allowance as the attach and main-state queries), and the
main-state query (<= 10 s, `UNIT_MAIN_STATE_ALLOWANCE`). At the 100 ms
production sample the window is about 60.3 s worst case before the
fail-close fires. The kernel's hard limits are enforced throughout that
window - `MemoryMax` on the unit, and `CPUQuota` and `TasksMax` on both the
unit and the supervisor's aggregate slice - so the delay grants no resource
headroom: a bridge cannot outrun its cgroup while the watchdog is between
samples. The watchdog thread's post-terminate lifetime is bounded by one
in-flight query (<= 10 s): every query it issues is a bounded subprocess,
and the thread returns immediately after the terminate it recorded.

## Multi-instance envelope (linux)

The per-supervisor budget (processes=16, startup_concurrency=4 at the
default) and the per-exchange startup bound are per supervisor: two
product instances (two supervisors) in one process share the one
systemd user manager, and the containment, slice, and unit identity
counters are process-global, so each instance's units and slice are
distinct and each holds its own 16-process budget. The ungated
two-supervisor witness (tests/envelope.rs, PLR-R11) activates 8
plugins on each supervisor and runs both full-budget reloads
concurrently - 8 candidates beside 8 published on each, 32 live plugin
processes on the one user manager at the peak - and refuses the ninth
declaration at admission on each, naming the per-supervisor reload-
safe bound. It measures and prints the per-exchange elapsed of both
supervisors and asserts no aggregate bound: the per-supervisor budget
is not a deployment restriction, and the product claims no N-instance
activation capacity beyond the witnessed two-supervisor full reload -
concurrent activation across supervisors shares the one user manager,
so per-supervisor bounds do not compose into an aggregate guarantee.

## Memory contract (linux)

- The observed-RSS **threshold** is `BridgeLimits.rss_bytes`. The cgroup
  watchdog (100 ms production sample, `limits.sample_millis`) compares the
  unit's `memory.current` against it and terminates on
  `memory > threshold` with a message naming the observed value and the
  threshold: `plugin bridge RSS <observed> exceeded the observed <T>-byte
  termination threshold`.
- The **hard cgroup ceiling** is no longer the threshold: it is
  `threshold + MEMORY_MAX_HEADROOM_BYTES` (144 MiB, named constant in
  `crates/grapt-plugin/src/protocol.rs`). At the base the ceiling equaled
  the threshold, and the watchdog evaluates its OOM branch before its
  threshold branch, so any ceiling hit became the first attributable cause of
  death and preempted the observed threshold. Field: the CCM battery (run
  20260926000830-85b6f235c07245de) red the rss witness at its explicit
  128 MiB limit with the boundary message, and a PSF battery at
  loadavg ~10 hit the default 256 MiB threshold-as-ceiling on 22/27 spawns.
- The **headroom** is the stated margin between threshold and ceiling.
  **Invariant:** for every spawn, `threshold < MemoryMax` always, with the gap
  probe-derived (review note, condition a). Value: the 28-parallel Node v24.21.0 startup-peak probe
  (receipt `receipts/plrrss-28.json`: 28/28 units peaked at 11.8-13.4 MiB,
  0 OOM events, under a non-interfering 1 GiB ceiling) plus the stated
  128 MiB margin, rounded up to a 16 MiB multiple: 13.4 + 128 = 141.4 ->
  **144 MiB** (final; the 256 MiB provisional stood until the probe ran, and
  the probe allows the lower value). The probe shows the startup charge is
  bounded (~13.4 MiB even at 28-parallel), so the field crossings are
  load transients — the headroom covers the charge a bridge can add between
  the watchdog sampling the threshold and the ceiling engaging, and bounds a
  runaway bridge even if the watchdog stalls. The observed-RSS threshold is
  the first trigger for any charge that grows at a pace the 50 ms watchdog
  can sample (the rss witness's paced allocation keeps inter-sample growth
  far below the gap); the hard ceiling remains a BOUNDED backstop for faster
  growth, not an unlimited allowance — and PLR103 proves the distinction
  (headroom 0 collapses the ceiling onto the threshold and the witness reds
  with the distinct cgroup-boundary outcome).
- The per-unit ceiling composes with the supervisor-wide slice ceiling
  (`SupervisorBudget.rss_bytes`, default 2 GiB, shared by every unit of one
  supervisor): the unit ceiling dominates while
  `threshold + headroom` stays under the slice budget; a user threshold whose
  ceiling exceeds the slice budget makes the slice the first boundary, as it
  already does at the base (where the unit ceiling is the threshold itself).
  No product change; the interaction is unchanged in kind.
- The OOM branch's message names the ceiling
  (`plugin bridge RSS reached the <ceiling>-byte hard cgroup memory boundary`)
  so the two outcomes are textually distinct.
- The rss witness allocates at a **paced rate** (16 MiB per 100 ms on linux)
  so the charge growth between 50 ms watchdog samples stays far below the
  headroom: the observed threshold is the first attributable trigger, and
  threshold-versus-ceiling attribution is not a kernel preemption race. The
  windows arm keeps the base witness's unbounded-rate tool: that platform owes
  its attribution to the runtime's allocation failure, unchanged.
- The remaining unit properties are unchanged: `MemorySwapMax=0`,
  `OOMPolicy=continue`, `KillMode=control-group`, `SendSIGKILL=yes`,
  `SuccessExitStatus=SIGKILL`, `RemainAfterExit=yes`.

## Runtime directory lifecycle (admission cleanup)

- Admission copies the verified runtime bytes into the service-owned private
  directory `<private_root>/<executable_sha256[0..16]>/<bridge_sha256[0..16]>`
  and counts each supervisor's claim on that directory process-wide. When the
  last claim of the process is released, the admitted copies and the empty
  directory levels the admission created are removed; a host root that still
  holds other content is left standing.
- Release happens at supervisor drop. An explicit `shutdown()` retires the
  generation but keeps the claim: the dogfood host keeps the supervisor
  across disable->enable cycles and prepares successor generations from the
  private copies (`PluginSupervisor::prepare` -> `PluginProcess::start` on
  the admitted executable), so removal at explicit shutdown would break
  re-activation. Every end-of-life path - an explicit shutdown followed by
  drop, or a drop alone - ends in the removal, because the drop calls
  `shutdown()` (generation retired) and then the fields drop in order.
- Ordering at removal time (Windows): the generation is already retired, the
  deny-delete holds on the copies are dropped first (field declaration order:
  the holds precede the admission guard), and only then is the directory
  removed.
- Removal failure is observable, never swallowed: the drop path and the
  admission error path report it on stderr (the admission error path also
  returns the primary error). The removal stops at the first non-empty or
  absent level, so a shared host root and other supervisors' claims are
  unaffected by one supervisor's cleanup.
- The test fixture owns its own roots: `runtime()`/`runtime_for()` in
  `tests/bridge.rs` return the runtime together with a guard that removes
  that test's `private_root` at test-scope end, so a failed or
  early-returned witness cannot leave a directory on the host. The product's
  admission cleanup is witnessed independently by
  `a_dropped_supervisor_removes_its_private_runtime_directory` and
  `a_shared_runtime_directory_survives_until_the_last_supervisor_drops`
  (tests/bridge.rs), and PLR104 keeps the drop-path removal pinned (the
  first witness reds if the removal is deleted).

## Scope and compatibility

- Linux only. The windows `terminate` path and other cfg platforms are
  unchanged (charter: out of scope unless the contract shows the same defect).
  The W2 seam witnesses are deliberately cross-platform (W2 seam
  ruling): the seam is a decision over an injected
  probe with the real bounded clock, so they run on any carrier.
- `BridgeLimits` gains exactly one field, `reap_millis`, with a default; every
  existing literal in the workspace uses `..DEFAULT_BRIDGE_LIMITS`, so no call
  site changes. The limits are serialized into the load exchange, but the JS
  bridge reads only `frame_bytes`/`registrations` from them, so the added
  field is inert across the protocol.
- `crates/grapt-plugin` is the only crate touched; `Cargo.toml`/`Cargo.lock`
  are unchanged.
- The test helper `node()` in `tests/bridge.rs` now resolves a PATH candidate
  via `process.execPath` (launcher-chain resolution). Kept per review
  note (b): without it, the named bridge tests
  fail on the box's default PATH, which resolves `node` to a launcher shim
  whose exec chain ends at the real runtime — the pinned shim bytes never
  match the unit MainPID's exe, so unit ownership reads as missing
  (evidence: `receipts/base-node-path.log`, the base helper run under the
  seat PATH). It is a test-only change; the product's own executable pinning
  is unchanged.
- Named test for (b): at the base, under the seat PATH
  (`which node` -> `/home/example/.vite-plus/bin/node`, a symlink whose exec
  chain ends at `js_runtime/node/24.21.0/bin/node`),
  `lifecycle::observed_rss_and_sustained_cpu_thresholds_terminate_the_owned_process`
  fails with `fixture containment ownership is missing` (receipt
  `receipts/base-node-path.log`, base worktree at `3ac2fb5ec0`, run 03:34Z).
  It is the only base test that asserts live-launch ownership (MainPID exe
  equals the pinned executable); the carrier PATH, where `which node` is
  already the real runtime, is unaffected by the helper change.
- QCU r3: the ownership inspector no longer reads `/proc/<pid>/exe`
  directly, which the filing namespace refuses with EACCES. The fixed
  helper in `tests/bridge/unit_identity.rs` runs only
  `/usr/bin/readlink -e /proc/<pid>/exe` in its own collected user unit,
  brackets it with the inspected unit's Id, InvocationID, MainPID and
  process start time, and proves the helper unit absent on every path. The
  equality with the pinned executable is unchanged. The user session bus
  stays reachable from the namespace, so the namespace alone is not
  confinement; source custody rests on the external watcher and the Git
  identity and diff checks.
