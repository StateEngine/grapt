//! Spawn, contain, observe, and reap one bridge process.

#[cfg(target_os = "linux")]
use super::validation::CommandEnvironment as _;
use super::*;

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub(super) const UNSUPPORTED_PLATFORM: &str = "plugin supervision is unsupported on this platform";

// Built on every Unix so the reclaim authority's compile-fail witness can
// name `RecordedGroup`; off macOS nothing else uses it.
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) mod journal;
#[cfg(target_os = "linux")]
pub(super) mod linux;
#[cfg(target_os = "macos")]
pub(super) mod macos;
// Built on every Unix so the reclaim authority's compile-fail witness can
// name `RecordedGroup`; off macOS nothing else uses it.
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) mod signal;
// Built on every Unix so the reclaim authority's compile-fail witness can
// name `RecordedGroup`; off macOS nothing else uses it.
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) mod tree;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
use linux::{bridge_command, start_resource_watchdog};
#[cfg(windows)]
pub(super) use windows::spawn_bridge;

pub(super) struct Spawned {
    pub(super) input: BridgeInput,
    pub(super) output: Box<dyn Read + Send>,
    pub(super) control: ProcessControl,
}

pub(super) enum BridgeInput {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Direct(ChildStdin),
    #[cfg(windows)]
    Windows(Box<dyn Write + Send>),
}

impl BridgeInput {
    pub(super) fn write(&mut self, bytes: &[u8]) -> Result<(), PluginError> {
        match self {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Direct(input) => input
                .write_all(bytes)
                .and_then(|_| input.flush())
                .map_err(|error| PluginError::Protocol(error.to_string())),
            #[cfg(windows)]
            Self::Windows(input) => input
                .write_all(bytes)
                .and_then(|_| input.flush())
                .map_err(|error| PluginError::Protocol(error.to_string())),
            #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
            _ => {
                let _ = bytes;
                Err(PluginError::Runtime(UNSUPPORTED_PLATFORM.to_owned()))
            }
        }
    }
}

pub(super) const NODE_ARGUMENTS: [&str; 5] = [
    "--permission",
    "--disable-proto=throw",
    "--no-addons",
    "--max-old-space-size=128",
    "--max-semi-space-size=8",
];

#[cfg(target_os = "linux")]
pub(super) fn spawn_bridge(
    _runtime: &VerifiedRuntime,
    containment: &AggregateContainment,
    limits: BridgeLimits,
    generation: PluginGenerationId,
    instance: u64,
    access: &super::isolation::ProcessAccess,
) -> Result<Spawned, PluginError> {
    let (mut command, unit) = bridge_command(containment, limits, generation, instance, access)?;
    let spawned = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn_with_required_environment();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            linux::clean_unit(&unit);
            return Err(PluginError::Spawn(error.to_string()));
        }
    };
    let pipes = child.stdin.take().zip(child.stdout.take());
    let control = ProcessControl {
        child: Arc::new(Mutex::new(Some(child))),
        violation: Arc::new(Mutex::new(None)),
        exit_observed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        containment: ProcessContainment::Systemd {
            unit: Arc::from(unit),
        },
        cleanup: Duration::from_millis(limits.cleanup_millis),
        reap: Duration::from_millis(limits.reap_millis),
        memory_max_bytes: linux::memory_max_bytes(limits.rss_bytes),
    };
    let Some((input, output)) = pipes else {
        let _ = control.terminate();
        return Err(PluginError::Spawn(
            "bridge pipes were not established".to_owned(),
        ));
    };
    if let Err(error) = start_resource_watchdog(control.clone(), 0, limits) {
        let _ = control.terminate();
        return Err(error);
    }
    Ok(Spawned {
        input: BridgeInput::Direct(input),
        output: Box::new(output),
        control,
    })
}

#[cfg(target_os = "macos")]
pub(super) fn spawn_bridge(
    runtime: &VerifiedRuntime,
    containment: &AggregateContainment,
    limits: BridgeLimits,
    _generation: PluginGenerationId,
    instance: u64,
    access: &super::isolation::ProcessAccess,
) -> Result<Spawned, PluginError> {
    macos::spawn_bridge(runtime, containment, limits, instance, access)
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub(super) fn spawn_bridge(
    _runtime: &VerifiedRuntime,
    _containment: &AggregateContainment,
    _limits: BridgeLimits,
    _generation: PluginGenerationId,
    _instance: u64,
    _access: &super::isolation::ProcessAccess,
) -> Result<Spawned, PluginError> {
    Err(PluginError::Runtime(UNSUPPORTED_PLATFORM.to_owned()))
}

/// A recorded termination cause. `Resource` names a limit event that
/// explains the death; `Other` names evidence or telemetry loss, or an
/// invalidation. At stream end a recorded `Resource` violation keeps its
/// reason; everything else classifies the observed exit.
#[derive(Clone, Debug)]
pub(super) struct Violation {
    pub(super) kind: ViolationKind,
    pub(super) reason: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ViolationKind {
    Resource,
    Other,
}

/// The classified end of the bridge output stream. `Exited` and
/// `StillRunning` are the named outcomes the process state decided at
/// the stated bound; `Corrupt` carries the original stream error
/// verbatim for a process that broke the frame contract while alive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum StreamEnd {
    Exited(String),
    StillRunning(String),
    Corrupt(String),
}

#[derive(Clone)]
pub(super) enum ProcessContainment {
    #[cfg(target_os = "linux")]
    Systemd { unit: Arc<str> },
    #[cfg(windows)]
    Windows {
        group: Arc<processkit::ProcessGroup>,
    },
    /// The bridge leads its own process group; the processes the watchdog
    /// last saw in its tree, with their start times, are what a teardown
    /// may still kill once the leader is reaped.
    #[cfg(target_os = "macos")]
    Watchdog {
        /// The process group, led by the sentinel (SC-MAC-02b).
        group: u32,
        leader: u32,
        /// The leader's start: nothing that started earlier is signalled.
        floor: u64,
        tracked: Arc<Mutex<tree::Tracked>>,
        custody: macos::Custody,
        /// The launch's journal record, removed once the sentinel is reaped.
        record: Option<Arc<macos::Journalled>>,
    },
}

impl ProcessContainment {
    /// The containment's name in a survives-kill outcome.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn label(&self) -> String {
        match self {
            #[cfg(target_os = "linux")]
            Self::Systemd { unit } => unit.to_string(),
            #[cfg(target_os = "macos")]
            Self::Watchdog { group, .. } => format!("process group {group}"),
        }
    }
}

#[derive(Clone)]
pub(super) struct ProcessControl {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) child: Arc<Mutex<Option<Child>>>,
    pub(super) violation: Arc<Mutex<Option<Violation>>>,
    /// Sticky observation of the launcher's exit. The first observation
    /// is final: a reaped `Child` answers later status reads with ECHILD,
    /// which would otherwise report a dead process as running.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) exit_observed: Arc<std::sync::atomic::AtomicBool>,
    pub(super) containment: ProcessContainment,
    #[cfg(windows)]
    completion: ProcessCompletion,
    #[cfg(windows)]
    exited: Arc<std::sync::atomic::AtomicBool>,
    pub(super) cleanup: Duration,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) reap: Duration,
    /// The unit's MemoryMax ceiling: the rss threshold plus the ruled
    /// headroom (protocol::MEMORY_MAX_HEADROOM_BYTES), the value the
    /// systemd unit was created with. Every reason that names the hard
    /// cgroup boundary carries this byte count, so the boundary outcome
    /// has one canonical reason regardless of which evidence path
    /// records it first (PLR103).
    #[cfg(target_os = "linux")]
    pub(super) memory_max_bytes: u64,
}

#[cfg(windows)]
type ProcessCompletion = Arc<Mutex<Option<Receiver<Result<(), String>>>>>;

impl ProcessControl {
    /// Stop containment, then reap within the cleanup grace. A launcher that
    /// still lives at the grace is killed and given the separately stated
    /// post-kill reap bound; a launcher alive at that bound is its own named
    /// outcome, distinct from a reaped child and an unreadable status. An
    /// already observed exit skips the kill and the reap but never the
    /// containment teardown: with RemainAfterExit the unit keeps its cgroup
    /// until it is stopped, and a skipped stop leaves the unit loaded with
    /// its charge under the aggregate slice, where concurrent load turns the
    /// residue into a slice-level OOM the victim unit reports as its own
    /// boundary (PLR106). A supervisor that dies without reaching this
    /// leaves its units so until the next supervisor starts and reclaims
    /// them (`linux::reclaim_orphaned_containment`). macOS shares this path
    /// with its watchdog containment in place of the unit.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn terminate(&self) -> Result<(), PluginError> {
        let mut held = self.child.lock().map_err(|_| PluginError::Poisoned)?;
        if self.exit_observed.load(Ordering::Acquire) {
            let outcome = self.stop_containment();
            *held = None;
            return outcome;
        }
        let Some(child) = held.as_mut() else {
            return Ok(());
        };
        self.stop_containment()?;
        let deadline = tokio::time::Instant::now()
            .checked_add(self.cleanup)
            .ok_or(PluginError::Timeout("process cleanup"))?;
        let total_deadline = deadline
            .checked_add(self.reap)
            .ok_or(PluginError::Timeout("process cleanup"))?;
        let unit = self.containment.label();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.exit_observed.store(true, Ordering::Release);
                    *held = None;
                    return Ok(());
                }
                Ok(None) if tokio::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    child
                        .kill()
                        .map_err(|error| PluginError::Protocol(error.to_string()))?;
                    return self.reap_after_kill(&mut held, total_deadline, &unit);
                }
                Err(error) => return Err(PluginError::Protocol(error.to_string())),
            }
        }
    }

    /// Wait the post-kill bound for a killed launcher to be reaped. The bound
    /// is checked before every poll so the distinct survives-kill outcome
    /// cannot be masked by a reap racing the bound.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn reap_after_kill(
        &self,
        held: &mut Option<Child>,
        total_deadline: tokio::time::Instant,
        unit: &str,
    ) -> Result<(), PluginError> {
        let mut probe = || -> std::io::Result<bool> {
            let Some(child) = held.as_mut() else {
                return Err(std::io::Error::other(
                    "plugin process custody was lost before the reap bound",
                ));
            };
            child.try_wait().map(|status| status.is_some())
        };
        let outcome = post_kill_reap(&mut probe, total_deadline, unit, self.reap);
        if outcome.is_ok() {
            self.exit_observed.store(true, Ordering::Release);
            *held = None;
        }
        outcome
    }

    #[cfg(windows)]
    pub(super) fn terminate(&self) -> Result<(), PluginError> {
        let mut held = self.completion.lock().map_err(|_| PluginError::Poisoned)?;
        let Some(receiver) = held.as_ref() else {
            return Ok(());
        };
        self.stop_containment()?;
        let reaped = receiver
            .recv_timeout(self.cleanup)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => PluginError::Timeout("process cleanup"),
                RecvTimeoutError::Disconnected => {
                    PluginError::Protocol("plugin process completion receipt was lost".to_owned())
                }
            })?;
        *held = None;
        reaped.map_err(PluginError::Protocol)
    }

    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    pub(super) fn terminate(&self) -> Result<(), PluginError> {
        Ok(())
    }

    /// Record the processes the watchdog saw in the bridge's tree.
    #[cfg(target_os = "macos")]
    pub(super) fn track(&self, seen: &tree::Tracked) {
        let ProcessContainment::Watchdog { tracked, .. } = &self.containment;
        if let Ok(mut tracked) = tracked.lock() {
            tracked.clone_from(seen);
        }
    }

    pub(super) fn stop_containment(&self) -> Result<(), PluginError> {
        match &self.containment {
            #[cfg(target_os = "linux")]
            ProcessContainment::Systemd { unit } => linux::stop_unit(unit),
            #[cfg(windows)]
            ProcessContainment::Windows { group } => group
                .kill_all()
                .map_err(|error| PluginError::Protocol(error.to_string())),
            #[cfg(target_os = "macos")]
            ProcessContainment::Watchdog {
                group,
                leader,
                floor,
                tracked,
                custody,
                record,
            } => {
                let tracked = tracked.lock().map_err(|_| PluginError::Poisoned)?.clone();
                macos::kill_tree(
                    *group,
                    *leader,
                    *floor,
                    &tracked,
                    custody,
                    self.reap,
                    record.as_deref(),
                )
            }
            #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
            _ => Err(PluginError::Runtime(UNSUPPORTED_PLATFORM.to_owned())),
        }
    }

    pub(super) fn terminate_for(
        &self,
        kind: ViolationKind,
        reason: String,
    ) -> Result<(), PluginError> {
        self.record_violation(kind, reason)?;
        self.terminate()
    }

    pub(super) fn record_violation(
        &self,
        kind: ViolationKind,
        reason: String,
    ) -> Result<(), PluginError> {
        let mut violation = self.violation.lock().map_err(|_| PluginError::Poisoned)?;
        if violation.is_none() {
            *violation = Some(Violation { kind, reason });
        }
        Ok(())
    }

    pub(super) fn violation(&self) -> Option<String> {
        let recorded = self
            .violation
            .lock()
            .ok()
            .and_then(|value| value.as_ref().map(|violation| violation.reason.clone()));
        recorded.or_else(|| match self.containment_violation() {
            Ok(reason) => reason,
            Err(reason) => Some(reason),
        })
    }

    /// The recorded violation only. The live containment query never runs
    /// here, so a stream-end read cannot be renamed by a bookkeeping race
    /// that outlives the transient unit.
    pub(super) fn recorded_violation(&self) -> Option<Violation> {
        self.violation.lock().ok().and_then(|value| value.clone())
    }

    fn containment_violation(&self) -> Result<Option<String>, String> {
        match &self.containment {
            #[cfg(target_os = "linux")]
            ProcessContainment::Systemd { unit } => linux::containment_violation_evidence_at(
                "/usr/bin/systemctl",
                unit,
                self.memory_max_bytes,
            ),
            #[cfg(windows)]
            ProcessContainment::Windows { .. } => Ok(None),
            #[cfg(target_os = "macos")]
            ProcessContainment::Watchdog { .. } => Ok(None),
            #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
            _ => Err(UNSUPPORTED_PLATFORM.to_owned()),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn has_exited(&self) -> Result<bool, PluginError> {
        if self.exit_observed.load(Ordering::Acquire) {
            return Ok(true);
        }
        let mut child = self.child.lock().map_err(|_| PluginError::Poisoned)?;
        let exited = match child.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(_)) => true,
                Ok(None) => false,
                Err(error) => return Err(PluginError::Protocol(error.to_string())),
            },
            None => true,
        };
        if exited {
            self.exit_observed.store(true, Ordering::Release);
        }
        Ok(exited)
    }

    #[cfg(windows)]
    pub(super) fn has_exited(&self) -> Result<bool, PluginError> {
        Ok(self.exited.load(Ordering::Acquire))
    }

    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    pub(super) fn has_exited(&self) -> Result<bool, PluginError> {
        Ok(true)
    }
}

/// The post-kill reap as a decision over an injected child-status probe
/// (CONTRACT.md, W2). Production wraps the real `Child::try_wait` in
/// `reap_after_kill`; the seam witness injects a deterministic double.
/// `Ok(true)` means the child reported an exit, `Ok(false)` that it still
/// runs, and `Err` that the status is unreadable, custody loss included.
/// The bound is checked before every poll, so a reap racing the bound
/// cannot mask the distinct outcome: a probe that never reports an exit
/// runs the full bound and lands in the survives-kill outcome, named for
/// the containment unit.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn post_kill_reap(
    probe: &mut dyn FnMut() -> std::io::Result<bool>,
    total_deadline: tokio::time::Instant,
    unit: &str,
    reap: Duration,
) -> Result<(), PluginError> {
    loop {
        if tokio::time::Instant::now() >= total_deadline {
            return Err(PluginError::SurvivesKill(format!(
                "plugin bridge launcher for containment unit {unit} was killed but not reaped within the post-kill bound of {} ms",
                reap.as_millis()
            )));
        }
        match probe() {
            Ok(true) => return Ok(()),
            Ok(false) => thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(PluginError::Protocol(error.to_string())),
        }
    }
}

/// A child the supervisor holds unreaped, killed and reaped through its own
/// handle. The seam lets the bound be witnessed with a child that never exits.
#[cfg(any(target_os = "macos", test))]
pub(super) trait Reapable {
    fn kill(&mut self) -> std::io::Result<()>;
    /// Whether the child's exit has been reaped.
    fn try_wait(&mut self) -> std::io::Result<bool>;
}

#[cfg(target_os = "macos")]
impl Reapable for Child {
    fn kill(&mut self) -> std::io::Result<()> {
        Child::kill(self)
    }

    fn try_wait(&mut self) -> std::io::Result<bool> {
        Child::try_wait(self).map(|status| status.is_some())
    }
}

/// Kill and reap a held child within `bound`. It is released only once its
/// reap is observed; a failed kill, or a child that survives the bound
/// (`post_kill_reap`'s survives-kill outcome), is reported and stays held.
#[cfg(any(target_os = "macos", test))]
pub(super) fn release<R: Reapable>(
    held: &mut Option<R>,
    bound: Duration,
    label: &str,
) -> Result<(), PluginError> {
    let Some(child) = held.as_mut() else {
        return Ok(());
    };
    child
        .kill()
        .map_err(|error| PluginError::Protocol(format!("{label} could not be killed: {error}")))?;
    let deadline = tokio::time::Instant::now()
        .checked_add(bound)
        .ok_or(PluginError::Timeout("process cleanup"))?;
    post_kill_reap(&mut || child.try_wait(), deadline, label, bound)?;
    *held = None;
    Ok(())
}

/// Children a failed startup could not reap within its bound (review HOLD
/// on 7a2af7d3). They stay owned here, keyed by process instance, with the
/// admission the failed start was charged, so no successor is admitted
/// until each is reaped; the watchdog sweeps them every sample.
#[cfg(any(target_os = "macos", test))]
pub(super) struct Stranded<R> {
    entries: Mutex<Vec<StrandedStart<R>>>,
}

#[cfg(any(target_os = "macos", test))]
struct StrandedStart<R> {
    instance: u64,
    children: Vec<R>,
    admission: Option<ProcessBudgetSlot>,
}

#[cfg(any(target_os = "macos", test))]
impl<R: Reapable> Default for Stranded<R> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
        }
    }
}

#[cfg(any(target_os = "macos", test))]
impl<R: Reapable> Stranded<R> {
    /// Keep a failed start's unreaped children.
    pub(super) fn hold(&self, instance: u64, children: Vec<R>) -> Result<(), PluginError> {
        if children.is_empty() {
            return Ok(());
        }
        self.entries
            .lock()
            .map_err(|_| PluginError::Poisoned)?
            .push(StrandedStart {
                instance,
                children,
                admission: None,
            });
        Ok(())
    }

    /// Charge a failed start's admission to its stranded children. With none
    /// stranded the admission is handed back for the caller to release.
    pub(super) fn charge(
        &self,
        instance: u64,
        admission: ProcessBudgetSlot,
    ) -> Option<ProcessBudgetSlot> {
        let Ok(mut entries) = self.entries.lock() else {
            // Poisoned: keep the admission charged rather than admit a successor.
            std::mem::forget(admission);
            return None;
        };
        match entries
            .iter_mut()
            .find(|entry| entry.instance == instance && entry.admission.is_none())
        {
            Some(entry) => {
                entry.admission = Some(admission);
                None
            }
            None => Some(admission),
        }
    }

    /// One non-blocking pass: kill again, keep what is not yet reaped, and
    /// release a start's admission once all its children are reaped.
    pub(super) fn sweep(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            for entry in entries.iter_mut() {
                entry.children.retain_mut(|child| {
                    let _ = child.kill();
                    !matches!(child.try_wait(), Ok(true))
                });
            }
            entries.retain(|entry| !entry.children.is_empty());
        }
    }

    pub(super) fn held(&self) -> usize {
        self.entries
            .lock()
            .map(|entries| entries.iter().map(|entry| entry.children.len()).sum())
            .unwrap_or(usize::MAX)
    }
}

/// Kill and reap a failed start's children within `bound` each; any that
/// survive or cannot be killed are transferred to `stranded`, never dropped.
#[cfg(any(target_os = "macos", test))]
pub(super) fn abandon_children<R: Reapable>(
    children: Vec<R>,
    bound: Duration,
    stranded: &Stranded<R>,
    instance: u64,
) -> Result<(), PluginError> {
    let mut failures = Vec::new();
    let mut kept = Vec::new();
    for child in children {
        let mut held = Some(child);
        if let Err(error) = release(&mut held, bound, "abandoned bridge process") {
            failures.push(error.to_string());
        }
        kept.extend(held);
    }
    stranded.hold(instance, kept)?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(PluginError::Protocol(format!(
            "{}; held until reaped",
            failures.join("; ")
        )))
    }
}

/// Children a test owns: released within the bound on `finish`, and every
/// survivor kept and reported, never dropped as if it had ended.
#[cfg(test)]
pub(super) struct Held<R: Reapable>(pub(super) Vec<R>);

#[cfg(test)]
impl<R: Reapable> Held<R> {
    /// One non-blocking pass: drop every child whose reap is observed.
    pub(super) fn reap_some(&mut self) {
        self.0
            .retain_mut(|child| !matches!(child.try_wait(), Ok(true)));
    }

    /// Release every child within `bound`; survivors stay held and named.
    pub(super) fn finish(&mut self, bound: Duration) -> Result<(), PluginError> {
        let mut failures = Vec::new();
        let mut survivors = Vec::new();
        for child in self.0.drain(..) {
            let mut held = Some(child);
            if let Err(error) = release(&mut held, bound, "test-owned child") {
                failures.push(error.to_string());
            }
            survivors.extend(held);
        }
        self.0 = survivors;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(PluginError::Protocol(format!(
                "{} test-owned child(ren) survived: {}",
                self.0.len(),
                failures.join("; ")
            )))
        }
    }
}

#[cfg(test)]
impl<R: Reapable> Drop for Held<R> {
    /// The last resort after a failed assertion: release, and report any
    /// survivor loudly rather than silently.
    fn drop(&mut self) {
        if let Err(error) = self.finish(Duration::from_secs(5)) {
            eprintln!("grapt-plugin test cleanup: {error}");
        }
    }
}

/// Classify the end of the bridge output stream. The launcher's exit is
/// observed within the stated bound; an observed exit is classified as an
/// exit, and only a recorded `Resource` violation (a limit event that
/// explains the death) keeps its own reason. At the bound without an
/// exit, the outcome names the still-running process, or carries the
/// stream error verbatim. `recorded` is the recorded violation only,
/// never the live containment query, so a bookkeeping race cannot rename
/// the exit after the transient unit is gone.
pub(super) fn settle_stream_end_classification(
    recorded: Option<&Violation>,
    observe_exit: &mut dyn FnMut() -> bool,
    bound: Duration,
    stream_error: Option<String>,
) -> StreamEnd {
    let deadline = match tokio::time::Instant::now().checked_add(bound) {
        Some(deadline) => deadline,
        None => return stream_end_without_exit(stream_error),
    };
    loop {
        if observe_exit() {
            return StreamEnd::Exited(match recorded {
                Some(violation) if violation.kind == ViolationKind::Resource => {
                    violation.reason.clone()
                }
                _ => "plugin bridge exited without replying".to_owned(),
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return stream_end_without_exit(stream_error);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn stream_end_without_exit(stream_error: Option<String>) -> StreamEnd {
    match stream_error {
        Some(error) => StreamEnd::Corrupt(error),
        None => StreamEnd::StillRunning(
            "plugin bridge closed its output while the process was still running".to_owned(),
        ),
    }
}

#[cfg(test)]
#[path = "process/release_tests.rs"]
mod release_tests;

#[cfg(test)]
#[path = "process/process_tests.rs"]
mod tests;
