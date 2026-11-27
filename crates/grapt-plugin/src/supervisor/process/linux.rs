use super::super::validation::CommandEnvironment as _;
use super::*;
use std::sync::atomic::AtomicU64;

static NEXT_CONTAINMENT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) fn bridge_command(
    containment: &AggregateContainment,
    limits: BridgeLimits,
    generation: PluginGenerationId,
    _instance: u64,
    access: &super::super::isolation::ProcessAccess,
) -> Result<(Command, String), PluginError> {
    if limits.tasks == 0 {
        return Err(PluginError::Runtime(
            "plugin task containment bound is zero".to_owned(),
        ));
    }
    let unit = next_containment_unit(generation)?;
    let mut command = systemd_run_command(&unit, limits);
    command.arg(format!("--slice={}", containment.slice));
    access.command.append_to(&mut command);
    command.env_clear();
    Ok((command, unit))
}

fn next_containment_unit(generation: PluginGenerationId) -> Result<String, PluginError> {
    let containment_id = NEXT_CONTAINMENT_ID
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .map_err(|_| PluginError::Runtime("plugin containment identity overflowed".to_owned()))?;
    Ok(format!(
        "secode-plugin-{}-{}-{containment_id}",
        std::process::id(),
        generation.value()
    ))
}

/// Create the supervisor-owned parent slice that bounds every bridge unit.
pub(in crate::supervisor) fn configure_slice(
    slice: &str,
    budget: SupervisorBudget,
) -> Result<(), PluginError> {
    let memory = format!("MemoryMax={}", budget.rss_bytes);
    let cpu = format!("CPUQuota={}%", budget.cpu_percent);
    let tasks = format!("TasksMax={}", budget.tasks);
    let output = bounded_systemctl(&[
        "set-property",
        "--runtime",
        slice,
        &memory,
        "MemorySwapMax=0",
        &cpu,
        &tasks,
    ])
    .map_err(|error| PluginError::Spawn(error.to_string()))?;
    if !output.status.success() {
        return Err(PluginError::Spawn(format!(
            "plugin aggregate containment slice {slice} could not be established"
        )));
    }
    Ok(())
}

pub(in crate::supervisor) fn stop_slice(slice: &str) {
    let _ = bounded_systemctl(&["stop", slice]);
    let _ = bounded_systemctl(&["revert", slice]);
}

/// Kill every process in the unit's control group and stop the unit.
pub(super) fn stop_unit(unit: &str) -> Result<(), PluginError> {
    let service = format!("{unit}.service");
    let _ = bounded_systemctl(&["kill", "--kill-whom=all", "--signal=KILL", &service])
        .map_err(|error| PluginError::Protocol(error.to_string()))?;
    let stopped = bounded_systemctl(&["stop", "--no-block", &service])
        .map_err(|error| PluginError::Protocol(error.to_string()))?;
    if !stopped.status.success() && !unit_is_explicitly_absent(&service) {
        return Err(PluginError::Protocol(format!(
            "plugin containment unit {unit} could not be stopped"
        )));
    }
    let _ = bounded_systemctl(&["reset-failed", &service]);
    Ok(())
}

fn unit_is_explicitly_absent(service: &str) -> bool {
    let evidence = bounded_systemctl(&[
        "show",
        service,
        "--property=Id,LoadState,ActiveState,SubState,MainPID,ControlPID",
    ]);
    absent_unit_evidence(
        service,
        evidence
            .as_ref()
            .map(|output| (output.status.success(), output.stdout.as_slice()))
            .map_err(|_| ()),
    )
}

// A failed stop is idempotent only with complete evidence for this exact unit.
// Loaded/inactive is deliberately not enough: it could still retain a cgroup.
fn absent_unit_evidence(service: &str, evidence: Result<(bool, &[u8]), ()>) -> bool {
    let Ok((true, bytes)) = evidence else {
        return false;
    };
    if bytes.len() > 4096 {
        return false;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let identity = format!("Id={service}");
    let required = [
        identity.as_str(),
        "LoadState=not-found",
        "ActiveState=inactive",
        "SubState=dead",
        "MainPID=0",
        "ControlPID=0",
    ];
    text.lines().count() == required.len()
        && required
            .iter()
            .all(|expected| text.lines().any(|line| line == *expected))
}

fn bounded_systemctl(arguments: &[&str]) -> std::io::Result<std::process::Output> {
    let mut command = Command::new("/usr/bin/systemctl");
    command
        .arg("--user")
        .arg("--no-pager")
        .args(arguments)
        .env_clear();
    bounded_command_output(command).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("systemctl {}: {error}", arguments.join(" ")),
        )
    })
}

// Allow ten seconds for the shared user manager under parallel containment load.
// systemctl has no descendants with --no-pager. Poll on the bounded clock,
// then kill/reap before reading at most 4097 bytes; a full pipe cannot block
// the wait.
fn bounded_command_output(command: Command) -> std::io::Result<std::process::Output> {
    bounded_command_output_within(command, Duration::from_millis(10_000))
}

fn bounded_command_output_within(
    command: Command,
    wait: Duration,
) -> std::io::Result<std::process::Output> {
    bounded_command_output_capped(command, wait, 4096)
}

/// The same bounded run, reading at most `max_bytes` of stdout. The output
/// is read after exit, so `max_bytes` stays under the pipe's capacity.
fn bounded_command_output_capped(
    mut command: Command,
    wait: Duration,
    max_bytes: u64,
) -> std::io::Result<std::process::Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_with_required_environment()?;
    let deadline = tokio::time::Instant::now()
        .checked_add(wait)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "systemctl allowance overflows the clock",
            )
        })?;
    let status = (|| loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "systemctl allowance exhausted",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    })();
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("systemctl output missing"))?
        .take(max_bytes + 1)
        .read_to_end(&mut stdout)?;
    if stdout.len() as u64 > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "systemctl output exceeds bound",
        ));
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

/// Reclaim the containment a supervisor left behind when it died without
/// retiring it. A crash or SIGKILL skips every Drop, so its bridge units
/// stay active after their bridges exit (RemainAfterExit, PLR106), each
/// holding its cgroup charge and its `systemd-run --wait` launcher, and its
/// aggregate slice keeps its runtime properties. Units and slices are named
/// for the supervisor's pid. Before this supervisor starts its own, a unit
/// is stopped only when its owner pid is gone and it has no main process
/// (exited or failed); a slice only when its owner pid is gone and none of
/// that owner's units remains. Nothing whose owner is alive is touched,
/// whether or not the pid was reused. Best effort and bounded: a listing
/// that cannot be read reclaims nothing. Returns whether a listing was
/// read and judged; `false` means nothing was.
pub(in crate::supervisor) fn reclaim_orphaned_containment() -> bool {
    let mut command = Command::new("/usr/bin/systemctl");
    command
        .arg("--user")
        .arg("--no-pager")
        .arg("show")
        .arg("secode-plugin-*.service")
        .arg("secode_plugins_*.slice")
        .arg("--property=Id,MainPID")
        .env_clear();
    // Read after exit, so the cap stays under a pipe's capacity; a listing
    // that outgrows a smaller pipe stalls, times out and reclaims nothing.
    let Ok(output) = bounded_command_output_capped(command, UNIT_MAIN_STATE_ALLOWANCE, 60 * 1024)
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let Ok(listing) = std::str::from_utf8(&output.stdout) else {
        return false;
    };
    let own = std::process::id();
    // Gone only on a definite answer: an unreadable /proc entry is alive.
    let gone = |owner: u32| {
        owner != own && matches!(Path::new(&format!("/proc/{owner}")).try_exists(), Ok(false))
    };
    let (units, slices) = orphaned_containment(listing);
    // Owners that still have a unit here keep their slice.
    let mut kept = std::collections::BTreeSet::new();
    for (unit, owner, main_pid) in units {
        if gone(owner) && main_pid == Some(0) && stop_unit(&unit).is_ok() {
            continue;
        }
        kept.insert(owner);
    }
    for (slice, owner) in slices {
        if gone(owner) && !kept.contains(&owner) {
            let _ = bounded_systemctl(&["stop", &slice]);
            let _ = bounded_systemctl(&["revert", &slice]);
        }
    }
    true
}

/// The units (name without `.service`, owner pid, MainPID) and slices
/// (name, owner pid) of one `show` listing; names that do not carry an
/// owner pid in this crate's form are not ours to judge and are skipped.
/// Units as (name without `.service`, owner pid, MainPID).
type ListedUnits = Vec<(String, u32, Option<u32>)>;
/// Slices as (name, owner pid).
type ListedSlices = Vec<(String, u32)>;

fn orphaned_containment(listing: &str) -> (ListedUnits, ListedSlices) {
    let (mut units, mut slices) = (Vec::new(), Vec::new());
    for block in listing.split("\n\n") {
        let id = block.lines().find_map(|line| line.strip_prefix("Id="));
        let main_pid = block
            .lines()
            .find_map(|line| line.strip_prefix("MainPID="))
            .and_then(|pid| pid.parse::<u32>().ok());
        let Some(id) = id else {
            continue;
        };
        if let Some(unit) = id.strip_suffix(".service") {
            // Exactly secode-plugin-<pid>-<generation>-<containment id>.
            if let Some(owner) = unit
                .strip_prefix("secode-plugin-")
                .and_then(|rest| generated_owner(rest, '-', 3))
            {
                units.push((unit.to_owned(), owner, main_pid));
            }
        } else if let Some(owner) = id
            .strip_prefix("secode_plugins_")
            .and_then(|rest| rest.strip_suffix(".slice"))
            .and_then(|rest| generated_owner(rest, '_', 2))
        {
            // Exactly secode_plugins_<pid>_<sequence>.slice.
            slices.push((id.to_owned(), owner));
        }
    }
    (units, slices)
}

/// The owner pid of a generated name's numeric tail: exactly `parts`
/// non-empty decimal parts joined by `separator`, the first a pid.
fn generated_owner(rest: &str, separator: char, parts: usize) -> Option<u32> {
    let fields: Vec<&str> = rest.split(separator).collect();
    let numeric = fields.len() == parts
        && fields
            .iter()
            .all(|field| !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit()));
    numeric.then(|| fields[0].parse::<u32>().ok()).flatten()
}

/// Best-effort cleanup for a unit whose launcher never produced a child.
pub(super) fn clean_unit(unit: &str) {
    let _ = stop_unit(unit);
}

fn systemd_run_command(unit: &str, limits: BridgeLimits) -> Command {
    let mut command = Command::new("/usr/bin/systemd-run");
    command
        .arg("--user")
        .arg("--quiet")
        .arg("--wait")
        .arg("--pipe")
        .arg("--service-type=exec")
        .arg(format!("--unit={unit}"))
        .arg(format!(
            "--property=MemoryMax={}",
            memory_max_bytes(limits.rss_bytes)
        ))
        .arg("--property=MemorySwapMax=0")
        .arg("--property=OOMPolicy=continue")
        .arg("--property=SuccessExitStatus=SIGKILL")
        .arg("--property=RemainAfterExit=yes")
        .arg(format!("--property=CPUQuota={}%", limits.cpu_percent))
        .arg(format!("--property=TasksMax={}", limits.tasks))
        .arg("--property=KillMode=control-group")
        .arg("--property=SendSIGKILL=yes");
    command.env_clear();
    command
}

/// The unit's MemoryMax ceiling: the rss threshold plus the ruled
/// headroom (protocol::MEMORY_MAX_HEADROOM_BYTES). One helper for the
/// boundary arithmetic so the unit that is created, the control that
/// names the boundary, and the witness that configures its ceiling
/// cannot drift apart (LOW, consolidated review RETURN, 2026-09-27).
pub(super) fn memory_max_bytes(rss_threshold: u64) -> u64 {
    rss_threshold.saturating_add(crate::protocol::MEMORY_MAX_HEADROOM_BYTES)
}

/// The one canonical reason for the hard cgroup boundary outcome. The
/// byte count is the unit's configured MemoryMax - the rss threshold
/// plus the ruled headroom - so the kernel OOM at the ceiling is tied
/// to the headroom arithmetic the ruling fixed, no matter which
/// evidence path records it first: the cgroup telemetry counter delta
/// and the systemd verdict both name the same ceiling (PLR103).
pub(super) fn memory_boundary_reason(memory_max_bytes: u64) -> String {
    format!("plugin bridge RSS reached the {memory_max_bytes}-byte hard cgroup memory boundary")
}

pub(super) fn classify_systemd_oom_evidence(
    output: Result<(bool, &[u8]), ()>,
    memory_max_bytes: u64,
) -> Result<Option<String>, String> {
    const UNAVAILABLE: &str =
        "plugin containment OOM evidence was unavailable; the owned bridge was terminated fail-closed";
    let (success, stdout) = output.map_err(|()| UNAVAILABLE.to_owned())?;
    if !success {
        return Err(UNAVAILABLE.to_owned());
    }
    let properties = std::str::from_utf8(stdout).map_err(|_| UNAVAILABLE.to_owned())?;
    let oom_kills = properties
        .lines()
        .find_map(|line| line.strip_prefix("OOMKills="))
        .ok_or_else(|| UNAVAILABLE.to_owned())?
        .parse::<u64>()
        .map_err(|_| UNAVAILABLE.to_owned())?;
    let result = properties
        .lines()
        .find_map(|line| line.strip_prefix("Result="))
        .ok_or_else(|| UNAVAILABLE.to_owned())?;
    let load_state = properties
        .lines()
        .find_map(|line| line.strip_prefix("LoadState="))
        .ok_or_else(|| UNAVAILABLE.to_owned())?;
    // R2 (consolidated review HOLD, 2026-09-26): positive evidence first.
    // systemd's own verdict that the unit was OOM-killed stands under any
    // LoadState and whether or not the OOMKills counter is populated - a
    // poll racing the start or stop window reads Result=oom-kill beside a
    // not-loaded state or an unpopulated sentinel, and neither race may
    // suppress the verdict.
    if result == "oom-kill" {
        return Ok(Some(memory_boundary_reason(memory_max_bytes)));
    }
    // PLR-R5 (consolidated review HOLD, 2026-09-26): the LoadState
    // decides the counter's meaning, exhaustively - the pre-fix blanket
    // rule read every non-loaded state, modelled or not, as absence by
    // default. Loaded with a populated counter: zero is absence, a
    // retained count is OOM evidence even under a successful Result (the
    // kill's SIGKILL maps to success under SuccessExitStatus). Loaded
    // with the not-yet-populated UINT64_MAX sentinel: a monotonic kill
    // count can never physically reach u64::MAX, so the sentinel is
    // absence, read in the startup window before the counter is
    // populated. not-found: the unit does not exist or never loaded, so
    // there is no OOM evidence to attribute. stub: a valid transient
    // startup state whose counter is not yet meaningful - the shape
    // probe (1387 production-shape queries, 3 distinct shapes) found
    // zero OOM evidence in any non-loaded state, and fail-closing a
    // valid startup state would repeat the C5 class. Every other state
    // systemd can report - error, bad-setting, masked, merged, or an
    // unrecognised value - fails closed UNAVAILABLE naming the state,
    // kept distinct from absence and from the transport UNAVAILABLE
    // above.
    let state = match load_state {
        "loaded" if oom_kills != u64::MAX => {
            (oom_kills > 0).then(|| memory_boundary_reason(memory_max_bytes))
        }
        "loaded" => None,
        "not-found" => None,
        "stub" => None,
        other => {
            return Err(format!(
                "plugin containment OOM evidence is unavailable: the unit's LoadState is {other}"
            ))
        }
    };
    Ok(state)
}

/// The allowance for one main-process exit query: the same ten seconds
/// the shared user manager gets under parallel containment load, and the
/// same bounded subprocess shape (polled, killed, reaped, capped
/// stdout) as the ControlGroup query whose bound PLR111 witnesses.
const UNIT_MAIN_STATE_ALLOWANCE: Duration = Duration::from_millis(10_000);

/// PLR-R11 (consolidated review RETURN on 80f3c50e, 2026-09-27): the
/// shared decision over a unit's main-process exit evidence, with the
/// one cell the two watchdog branches legitimately differ on as a
/// parameter: `accept_not_found` admits LoadState not-found as a
/// settled exit. `failed` is included because a non-zero exit outside
/// SuccessExitStatus settles the unit failed - the process-loss shape
/// whose exit the c15-it2/c3-it2 lifecycle red hid under a telemetry
/// loss (shape probe on this box, exact production unit shape:
/// SubState=exited by 100 ms for the SIGKILL shape, SubState=failed by
/// 290 ms for the exit-71 shape, and the systemd-run --wait launcher
/// still running at 4 s in both). Everything else defers nothing: an
/// unsettled or unrecognised SubState, an unrecognised LoadState, a
/// missing property, overlong or non-UTF-8 stdout, a failed query, and
/// the transport error all answer false, so an unknown state fails
/// closed exactly as the pre-fix window did.
fn main_state_evidence(evidence: Result<(bool, &[u8]), ()>, accept_not_found: bool) -> bool {
    let Ok((true, bytes)) = evidence else {
        return false;
    };
    if bytes.len() > 4096 {
        return false;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let (Some(load_state), Some(sub_state)) = (
        text.lines()
            .find_map(|line| line.strip_prefix("LoadState=")),
        text.lines().find_map(|line| line.strip_prefix("SubState=")),
    ) else {
        return false;
    };
    (load_state == "loaded" || (accept_not_found && load_state == "not-found"))
        && matches!(sub_state, "exited" | "failed" | "dead")
}

/// The sampling-branch predicate (PLR-R8, unchanged by R11): the
/// watchdog already attached to this unit's cgroup, so a unit that has
/// since left (LoadState not-found, SubState dead) is a settled exit,
/// not a telemetry loss; the stream-end path owns the classification.
pub(super) fn main_process_exited_evidence(evidence: Result<(bool, &[u8]), ()>) -> bool {
    main_state_evidence(evidence, true)
}

/// The attach-branch predicate (PLR-R11): the attach exhausted its
/// bound without ever locating the unit's cgroup, so the unit read
/// here is the not-yet-created shape - and a never-created unit
/// answers LoadState not-found, SubState dead, OOMKills max,
/// Result=success, rc 0 (review, verified on this box). Deferring
/// that shape is the R10 shared-predicate defect (finding
/// PLR-R11-72933DEA-ATTACH-NOTFOUND-FAILS-OPEN): under the measured
/// activation funnel a unit activating just after the attach bound
/// would run unwatched. Here not-found is ambiguous - never started,
/// or gone mid-attach - and ambiguity fails closed; only a settled
/// terminal SubState under LoadState loaded defers to the stream-end
/// path.
pub(super) fn attach_process_exited_evidence(evidence: Result<(bool, &[u8]), ()>) -> bool {
    main_state_evidence(evidence, false)
}

/// One bounded main-process exit query for the unit's service (PLR-R8):
/// the same bounded subprocess stack as the ControlGroup query, reading
/// only the two properties the decision consumes.
fn unit_main_state_query(unit: &str) -> std::io::Result<std::process::Output> {
    unit_main_state_query_at("/usr/bin/systemctl", &format!("{unit}.service"))
}

/// The same query against an injectable systemctl path - the PLR112
/// witness seam. The production path is hardcoded to /usr/bin/systemctl
/// and a PATH stub cannot intercept it, so the witness proves the bound
/// on the query itself by running this seam against a stalled stub, the
/// same seam law as the ControlGroup query (PLR111).
pub(super) fn unit_main_state_query_at(
    systemctl_path: &str,
    service: &str,
) -> std::io::Result<std::process::Output> {
    let mut command = Command::new(systemctl_path);
    command
        .arg("--user")
        .arg("--no-pager")
        .arg("show")
        .arg(service)
        .arg("--property=LoadState,SubState")
        .env_clear();
    bounded_command_output_within(command, UNIT_MAIN_STATE_ALLOWANCE).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("systemctl main-state query for {service}: {error}"),
        )
    })
}

/// The containment (OOM-verdict) query itself is the bounded subprocess
/// (R9, the disclosed correction to the pre-review bound formula):
/// the same bounded stack and allowance as the attach and main-state
/// queries, so a stalled user manager cannot hold the watchdog's
/// fail-close window unbounded. A timeout-less `.output()` here is the
/// exact shape PLR111 removed from the sibling ControlGroup query, and
/// it predates the R8 correction (introduced with the OOM verdict). The
/// production path is hardcoded to /usr/bin/systemctl and a PATH stub
/// cannot intercept it, so the witness proves the bound on this seam,
/// the same seam law as PLR111 and PLR112.
pub(super) fn containment_violation_evidence_at(
    systemctl_path: &str,
    unit: &str,
    memory_max_bytes: u64,
) -> Result<Option<String>, String> {
    let mut command = Command::new(systemctl_path);
    command
        .arg("--user")
        .arg("--no-pager")
        .arg("show")
        .arg(format!("{unit}.service"))
        .arg("--property=OOMKills")
        .arg("--property=Result")
        .arg("--property=LoadState")
        .env_clear();
    let output = bounded_command_output_within(command, UNIT_MAIN_STATE_ALLOWANCE);
    let evidence = match output.as_ref() {
        Ok(output) => Ok((output.status.success(), output.stdout.as_slice())),
        Err(_) => Err(()),
    };
    classify_systemd_oom_evidence(evidence, memory_max_bytes)
}

/// The decision over one live query: a settled terminal main-process
/// state defers the watchdog's fail-close to the stream-end path that
/// owns the exit classification; an unsettled, unknown, or unreadable
/// unit fails closed as before (PLR-R8).
fn unit_main_process_exited(unit: &str) -> bool {
    let Ok(output) = unit_main_state_query(unit) else {
        return false;
    };
    main_process_exited_evidence(Ok((output.status.success(), &output.stdout)))
}

/// The attach-branch decision over one live query (PLR-R11): the same
/// bounded query, decided by the attach predicate - not-found and
/// every unsettled or unreadable state fail the attach fail-close
/// closed.
fn attach_unit_main_process_exited(unit: &str) -> bool {
    let Ok(output) = unit_main_state_query(unit) else {
        return false;
    };
    attach_process_exited_evidence(Ok((output.status.success(), &output.stdout)))
}

pub(super) fn start_resource_watchdog(
    control: ProcessControl,
    _process_id: u32,
    limits: BridgeLimits,
) -> Result<(), PluginError> {
    let ProcessContainment::Systemd { unit } = &control.containment;
    start_cgroup_watchdog(control.clone(), Arc::clone(unit), limits)
}

/// One cgroup telemetry reading, taken once per watchdog decision.
#[derive(Clone, Copy, Debug)]
struct CgroupSample {
    memory: u64,
    oom: u64,
    throttled: u64,
}

/// The production sample reader: every field is read from the attached
/// cgroup, and any unreadable or unparsable field makes the whole sample
/// missing.
fn read_cgroup_sample(cgroup: &Path) -> Option<CgroupSample> {
    let memory = std::fs::read_to_string(cgroup.join("memory.current"))
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    let oom = read_named_counter(&cgroup.join("memory.events"), "oom");
    let throttled = read_named_counter(&cgroup.join("cpu.stat"), "nr_throttled");
    Some(CgroupSample {
        memory: memory?,
        oom: oom?,
        throttled: throttled?,
    })
}

fn start_cgroup_watchdog(
    control: ProcessControl,
    unit: Arc<str>,
    limits: BridgeLimits,
) -> Result<(), PluginError> {
    start_cgroup_watchdog_sampling(control, unit, limits, read_cgroup_sample)
}

/// The watchdog with its per-decision sample reader as a private seam.
/// Production passes `read_cgroup_sample`; the PLR112 witness passes a
/// reader that delegates to it until the scenario injects absence at
/// this boundary. Attach, baselines, OOM-first ordering, launcher
/// liveness, the settled-state query and the missing count are the same
/// code on both paths.
#[cfg_attr(not(windows), expect(clippy::too_many_lines, reason = "legacy"))]
fn start_cgroup_watchdog_sampling(
    control: ProcessControl,
    unit: Arc<str>,
    limits: BridgeLimits,
    mut sampler: impl FnMut(&Path) -> Option<CgroupSample> + Send + 'static,
) -> Result<(), PluginError> {
    thread::Builder::new()
        .name(format!("plugin-resource-{unit}"))
        .spawn(move || {
            let sample = Duration::from_millis(limits.sample_millis);
            // R1: attach is bounded by the generation's own startup bound
            // (see locate_cgroup), never a fixed attempt count.
            let attach_bound = Duration::from_millis(limits.startup_millis);
            let Some(cgroup) = locate_cgroup(&control, &unit, sample, attach_bound) else {
                // A dead child is not an anomaly: the stream-end path owns
                // the classification, so the watchdog records nothing for it.
                if control.has_exited().unwrap_or(true) {
                    return;
                }
                match control.containment_violation() {
                    Ok(Some(reason)) => {
                        let _ = control.terminate_for(ViolationKind::Resource, reason);
                        return;
                    }
                    Err(reason) => {
                        let _ = control.terminate_for(ViolationKind::Other, reason);
                        return;
                    }
                    Ok(None) => {}
                }
                // PLR-R11 (successor of PLR-R10, the attach-branch twin
                // of PLR-R8, corrected by the review RETURN on 80f3c50e):
                // a settled terminal main-process exit is not a
                // telemetry loss, but ONLY under LoadState loaded (the
                // attach predicate). This branch fires when the attach
                // exhausted its bound on a unit whose ControlGroup can
                // no longer be located while the launcher is still
                // running - and a unit that never created reads
                // not-found/dead with rc 0 on this carrier, the shape
                // the R10 shared predicate deferred and that left a unit
                // activating just after the attach bound unwatched
                // (finding PLR-R11-72933DEA-ATTACH-NOTFOUND-FAILS-OPEN).
                // The decision is checked once here - no sample window
                // in this branch - with the order unchanged: the
                // dead-child check and the OOM verdict above still win
                // (an OOM-killed main is a violation, never a silent
                // exit); an unsettled, not-found, unknown, or
                // unreadable unit falls through to the fail-close below.
                if attach_unit_main_process_exited(&unit) {
                    return;
                }
                let _ = control.terminate_for(
                    ViolationKind::Other,
                    "plugin cgroup telemetry was unavailable; the owned bridge was terminated fail-closed"
                        .to_owned(),
                );
                return;
            };
            // The cgroup is unique to this bridge generation, so every OOM event belongs to
            // it. Starting from the first observed value would erase a child OOM that races
            // watchdog attachment while the main process remains alive.
            let mut prior_oom = 0u64;
            let mut prior_throttled =
                read_named_counter(&cgroup.join("cpu.stat"), "nr_throttled").unwrap_or(0);
            let mut hot = 0u16;
            let mut missing = 0u8;
            loop {
                thread::sleep(sample);
                let Some(CgroupSample {
                    memory,
                    oom,
                    throttled,
                }) = sampler(&cgroup)
                else {
                    // A dead child is not an anomaly: the stream-end path
                    // owns the classification, so the watchdog records
                    // nothing for it.
                    if control.has_exited().unwrap_or(true) {
                        return;
                    }
                    match control.containment_violation() {
                        Ok(Some(reason)) => {
                            let _ = control.terminate_for(ViolationKind::Resource, reason);
                            return;
                        }
                        Err(reason) => {
                            let _ = control.terminate_for(ViolationKind::Other, reason);
                            return;
                        }
                        Ok(None) => {}
                    }
                    // PLR-R8: a known exit is not a telemetry loss. The
                    // launcher's exit lags the unit's own state (shape
                    // probe: the unit settled 100-290 ms after the main
                    // process left while the launcher was still running
                    // at 4 s), and in that window the emptied unit cgroup
                    // is pruned, so this branch is the watchdog's only
                    // view. A settled terminal state defers the
                    // classification to the stream-end path exactly as
                    // the dead-child path above does; the OOM verdict
                    // above still wins - an OOM-killed main is a
                    // violation, never a silent exit; and an unsettled,
                    // unknown, or unreadable unit falls through to the
                    // fail-close below.
                    if unit_main_process_exited(&unit) {
                        return;
                    }
                    missing = missing.saturating_add(1);
                    if missing >= 3 {
                        let _ = control.terminate_for(
                            ViolationKind::Other,
                            "plugin cgroup telemetry became unavailable; the owned bridge was terminated fail-closed"
                                .to_owned(),
                        );
                        return;
                    }
                    continue;
                };
                missing = 0;
                if oom > prior_oom {
                    let _ = control.terminate_for(
                        ViolationKind::Resource,
                        memory_boundary_reason(control.memory_max_bytes),
                    );
                    return;
                }
                prior_oom = oom;
                if memory > limits.rss_bytes {
                    let _ = control.terminate_for(ViolationKind::Resource, format!(
                        "plugin bridge RSS {memory} exceeded the observed {}-byte termination threshold",
                        limits.rss_bytes
                    ));
                    return;
                }
                hot = if throttled > prior_throttled {
                    hot.saturating_add(1)
                } else {
                    0
                };
                prior_throttled = throttled;
                if hot >= limits.cpu_violation_samples {
                    let _ = control.terminate_for(ViolationKind::Resource, format!(
                        "plugin bridge CPU exhausted its {}% cgroup quota for {} samples; the owned process tree was terminated",
                        limits.cpu_percent, limits.cpu_violation_samples
                    ));
                    return;
                }
            }
        })
        .map(|_| ())
        .map_err(|error| PluginError::Spawn(error.to_string()))
}

// R1/R6 (consolidated review HOLD + ruling, 2026-09-26): attach is
// bounded by the generation's own startup bound, not a fixed attempt count,
// and the bound is enforced per call. The pre-fix fixed 30 x sample window
// (~3 s at the 100 ms default) gave up while a healthy generation's cgroup
// was still materialising under D-Bus contention, and its timeout-less
// systemctl .output() made even the polled window unbounded: a stalled D-Bus
// query could hold the attach past the startup bound forever. Now every
// query runs as a bounded subprocess that waits at most the remaining budget
// and is killed and reaped on overrun, the sleep is clamped to the remaining
// time, and a stalled query fails the attach closed at the deadline. The
// total attach is bounded by startup_millis plus one 20 ms query-poll tick
// of scheduling slack; CONTRACT.md states the same bound.
fn locate_cgroup(
    control: &ProcessControl,
    unit: &str,
    sample: Duration,
    bound: Duration,
) -> Option<PathBuf> {
    locate_cgroup_with(control, unit, sample, bound, &bounded_show_control_group)
}

fn locate_cgroup_with(
    control: &ProcessControl,
    unit: &str,
    sample: Duration,
    bound: Duration,
    query: &impl Fn(&str, Duration) -> Result<Option<String>, std::io::Error>,
) -> Option<PathBuf> {
    let deadline = tokio::time::Instant::now().checked_add(bound)?;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match query(unit, remaining) {
            Ok(Some(relative)) if !relative.is_empty() => {
                return Some(Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')));
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
                // The query outlived the remaining budget: it was killed
                // and reaped by the bound, and the attach fails closed at
                // the deadline instead of waiting on a stalled query.
                return None;
            }
            _ => {}
        }
        if control.has_exited().unwrap_or(true) {
            return None;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        thread::sleep(sample.min(remaining));
    }
}

/// One ControlGroup query for the unit's service, bounded to `wait`: the
/// subprocess is polled and killed and reaped on overrun; the capped stdout
/// read keeps the reap wait unblockable (a full pipe cannot hold the child).
/// A successful empty answer (the unit has no control group yet) is
/// Ok(None), kept distinct from a failure and from a timeout.
fn bounded_show_control_group(
    unit: &str,
    wait: Duration,
) -> Result<Option<String>, std::io::Error> {
    bounded_show_control_group_at("/usr/bin/systemctl", unit, wait)
}

/// The same query against an injectable systemctl path - the PLR111
/// witness seam. The production path is hardcoded to /usr/bin/systemctl
/// and a PATH stub cannot intercept it, so the witness proves the bound
/// on the query itself, not only on the attach loop that consumes its
/// verdict, by running this seam against a stalled stub.
fn bounded_show_control_group_at(
    systemctl_path: &str,
    unit: &str,
    wait: Duration,
) -> Result<Option<String>, std::io::Error> {
    let mut command = Command::new(systemctl_path);
    command
        .arg("--user")
        .arg("show")
        .arg(format!("{unit}.service"))
        .arg("--property=ControlGroup")
        .arg("--value")
        .env_clear();
    let output = bounded_command_output_within(command, wait).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("systemctl ControlGroup query for unit {unit}: {error}"),
        )
    })?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "systemctl show failed for unit {unit}"
        )));
    }
    let value = String::from_utf8(output.stdout).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("systemctl ControlGroup output for unit {unit} is not UTF-8"),
        )
    })?;
    let relative = value.trim();
    Ok((!relative.is_empty()).then(|| relative.to_owned()))
}

fn read_named_counter(path: &Path, name: &str) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            (fields.next()? == name)
                .then(|| fields.next()?.parse::<u64>().ok())
                .flatten()
        })
}

#[cfg(test)]
mod query_seam;

#[cfg(test)]
mod reclaim_tests;

#[cfg(test)]
mod tests;
