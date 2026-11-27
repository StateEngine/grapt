//! macOS bridge containment (SC-MAC-02): a process group per bridge and one
//! sampling watchdog per supervisor. It is watchdog-enforced, not
//! kernel-enforced; [`super::tree`] holds every decision and states exactly
//! what is and is not contained.

use super::journal::{self, Journal, Record, Recorded};
use super::signal::{self, Own, Refusal, Seen, Sent, Target};
use super::tree::{self, Aggregate, Entry, Table, Tracked, Verdict, Watch};
use super::*;

use nix::errno::Errno;
use nix::sys::signal::{kill, killpg, Signal};
use nix::unistd::{getpgid, getpgrp, getuid, Pid as NixPid};
use std::os::unix::process::CommandExt as _;
use std::sync::atomic::AtomicBool;
use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

/// One snapshot of the process table, with each process's group.
fn snapshot(system: &mut System) -> Table {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory().with_cpu(),
    );
    system
        .processes()
        .iter()
        .map(|(pid, process)| {
            let raw = pid.as_u32();
            let group = i32::try_from(raw)
                .ok()
                .and_then(|raw| getpgid(Some(NixPid::from_raw(raw))).ok())
                .and_then(|group| u32::try_from(group.as_raw()).ok());
            let entry = Entry {
                pid: raw,
                parent: process.parent().map(|parent| parent.as_u32()),
                group,
                started: process.start_time(),
                rss: process.memory(),
                cpu_millis: process.accumulated_cpu_time(),
                readable: process.memory() > 0 || process.status() == ProcessStatus::Zombie,
            };
            (raw, entry)
        })
        .collect()
}

/// The kernel's kills; "no such process" is not a failure.
struct Kernel;

impl signal::Signals for Kernel {
    fn kill(&mut self, pid: i32) -> Result<(), String> {
        sent(kill(NixPid::from_raw(pid), Signal::SIGKILL))
    }

    fn kill_group(&mut self, group: i32) -> Result<(), String> {
        sent(killpg(NixPid::from_raw(group), Signal::SIGKILL))
    }
}

fn sent(outcome: nix::Result<()>) -> Result<(), String> {
    match outcome {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// Identity as the process table reads it now.
struct Live;

impl signal::Probe for Live {
    fn observe(&self, pid: u32) -> Seen {
        let mut system = System::new();
        let target = sysinfo::Pid::from_u32(pid);
        system.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
        if let Some(process) = system.process(target) {
            return Seen::Alive {
                started: process.start_time(),
                parent: process.parent().map(|parent| parent.as_u32()),
            };
        }
        // The table cannot describe a zombie: ask the kernel twice.
        let Ok(raw) = i32::try_from(pid) else {
            return Seen::Unreadable;
        };
        let lookup = |result: nix::Result<()>| match result {
            Ok(()) => signal::Lookup::Found,
            Err(Errno::ESRCH) => signal::Lookup::Missing,
            Err(_) => signal::Lookup::Failed,
        };
        let null_signal = lookup(kill(NixPid::from_raw(raw), None));
        let group = lookup(getpgid(Some(NixPid::from_raw(raw))).map(|_| ()));
        signal::unlisted(null_signal, group)
    }

    fn group_of(&self, pid: u32) -> Option<u32> {
        let pid = i32::try_from(pid).ok()?;
        let group = getpgid(Some(NixPid::from_raw(pid))).ok()?;
        u32::try_from(group.as_raw()).ok()
    }
}

/// The process table for reclaim: whole groups, and argv[0] by user.
struct LiveCensus;

impl signal::Probe for LiveCensus {
    fn observe(&self, pid: u32) -> Seen {
        signal::Probe::observe(&Live, pid)
    }

    fn group_of(&self, pid: u32) -> Option<u32> {
        signal::Probe::group_of(&Live, pid)
    }
}

impl journal::Census for LiveCensus {
    fn members(&self, group: u32) -> Option<Vec<u32>> {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        let reads = system.processes().keys().map(|pid| {
            let pid = pid.as_u32();
            let read = match i32::try_from(pid).map(|raw| getpgid(Some(NixPid::from_raw(raw)))) {
                Ok(Ok(found)) => u32::try_from(found.as_raw())
                    .map_or(journal::GroupRead::Unreadable, journal::GroupRead::In),
                Ok(Err(Errno::ESRCH)) => journal::GroupRead::Gone,
                _ => journal::GroupRead::Unreadable,
            };
            (pid, read)
        });
        journal::census_members(group, reads)
    }

    fn tagged(&self, tag: &str, uid: u32) -> Option<Vec<u32>> {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cmd(UpdateKind::Always)
                .with_user(UpdateKind::Always),
        );
        let mut tagged = Vec::new();
        for (pid, process) in system.processes() {
            if process.user_id().map(|user| **user) != Some(uid)
                || process.status() == ProcessStatus::Zombie
            {
                continue;
            }
            // A process of this user whose argv cannot be read could be the
            // sentinel: the census is then incomplete, never guessed.
            let first = process.cmd().first()?;
            if first.to_str() == Some(tag) {
                tagged.push(pid.as_u32());
            }
        }
        Some(tagged)
    }

    /// The table drops a zombie, but a null signal still finds one until it
    /// is reaped. Only ESRCH proves the PID is free.
    fn unreaped(&self, pid: u32) -> bool {
        i32::try_from(pid).map_or(true, |raw| {
            !matches!(kill(NixPid::from_raw(raw), None), Err(Errno::ESRCH))
        })
    }
}

/// The supervisor itself, never a target.
pub(super) fn own() -> Own {
    Own {
        pid: std::process::id(),
        group: u32::try_from(getpgrp().as_raw()).unwrap_or(0),
    }
}

/// Send every target through the gate, re-reading an unreadable process
/// until `settle`. A reused PID is not the bridge's and is skipped; anything
/// else refused is reported as unverified.
fn kill_all(targets: Vec<Target>, settle: Duration) -> Result<(), PluginError> {
    let own = own();
    let deadline = tokio::time::Instant::now()
        .checked_add(settle)
        .ok_or(PluginError::Timeout("process cleanup"))?;
    let mut unverified = Vec::new();
    for target in targets {
        match signal::signal_settled(target, own, &Live, &mut Kernel, deadline) {
            Ok(Sent::Signalled | Sent::Gone) | Err(Refusal::Reused(_)) => {}
            Err(refusal) => unverified.push(refusal.to_string()),
        }
    }
    if unverified.is_empty() {
        Ok(())
    } else {
        Err(PluginError::Protocol(format!(
            "plugin teardown refused unverified targets: {}",
            unverified.join("; ")
        )))
    }
}

/// The supervisor's custody of a bridge's process group: its sentinel, a
/// child of ours that leads the group and is never reaped until teardown. A
/// group exists while any member is unreaped, and POSIX never reuses a group
/// ID while its group exists, so while the sentinel is held the ID is
/// provably the bridge's, whether or not the bridge leader was reaped. The
/// sentinel outlives a crash of ours, so a successor can reclaim the group
/// through its journal record (`journal`).
pub(super) type Custody = Arc<Mutex<Option<Child>>>;

/// A launch's published record. It is retired only on a fresh census proof
/// that the launch ended (`journal::prove_ended`); this has no other way to
/// delete it.
#[derive(Debug)]
pub(in crate::supervisor) struct Journalled {
    journal: Arc<Journal>,
    record: Record,
}

impl Journalled {
    fn retire_if_ended(&self) -> Result<(), PluginError> {
        match journal::prove_ended(&self.record, &LiveCensus) {
            Some(proof) => self.journal.retire_record(proof),
            None => Err(PluginError::Runtime(format!(
                "plugin launch {} was not proven ended; its record is kept",
                self.record.nonce
            ))),
        }
    }
}

/// Kill a bridge's tree, then release its custody. While the sentinel is
/// held the group dies on its authority, with every descendant of the leader
/// outside the group now in the table; without it no group is signalled.
/// Every process the watchdog saw in the tree is killed only under its
/// recorded identity. The journal record goes only once the sentinel's reap
/// is observed: an unreleased sentinel stays reclaimable by a successor.
pub(super) fn kill_tree(
    group: u32,
    leader: u32,
    floor: u64,
    tracked: &Tracked,
    custody: &Custody,
    reap: Duration,
    record: Option<&Journalled>,
) -> Result<(), PluginError> {
    let mut sentinel = custody.lock().map_err(|_| PluginError::Poisoned)?;
    let holder = sentinel.as_ref().map(Child::id);
    let mut targets = Vec::new();
    if let Some(holder) = holder {
        targets.push(Target::Group { group, holder });
        let table = snapshot(&mut System::new());
        targets.extend(
            tree::members_of(&table, group, leader)
                .iter()
                .filter(|entry| entry.group != Some(group) && entry.pid != holder)
                .map(|entry| Target::Process {
                    identity: tree::identity(&table, entry),
                    floor,
                }),
        );
    }
    // The leader is our own child: it dies with its group, or through its own
    // handle, and is reaped there. By PID it would read as an unreaped zombie,
    // or, once reaped, could be someone else.
    targets.extend(
        tracked
            .values()
            .filter(|identity| identity.pid != leader)
            .map(|identity| Target::Process {
                identity: *identity,
                floor,
            }),
    );
    let killed = kill_all(targets, reap);
    let held = sentinel.is_some();
    // Custody is released only once the sentinel's reap is observed, within
    // the declared bound; a sentinel that survives it stays held and named.
    let released = release(
        &mut *sentinel,
        reap,
        &format!("sentinel of process group {group}"),
    );
    // The record is retired only after a successful group kill and an
    // observed sentinel reap, and then only on a fresh proof that the group
    // is gone; any doubt keeps it for a successor's reclaim.
    let retired = match record {
        Some(record) if held && killed.is_ok() && released.is_ok() => record.retire_if_ended(),
        _ => Ok(()),
    };
    both(both(killed, released), retired)
}

/// Both outcomes, neither dropped.
fn both(
    first: Result<(), PluginError>,
    second: Result<(), PluginError>,
) -> Result<(), PluginError> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(PluginError::Protocol(format!("{first}; {second}"))),
    }
}

/// A spawn that failed after its launch: the bridge leader and sentinel are
/// killed and reaped within the reap bound, and whatever survives stays owned
/// by the supervisor's stranded custody (`Stranded`), never dropped. The
/// journal record is removed only when both ended; otherwise a successor can
/// still reclaim the group.
fn abandon(
    containment: &AggregateContainment,
    instance: u64,
    children: Vec<Child>,
    reap: Duration,
    error: PluginError,
) -> PluginError {
    let stranded = &containment.watchdog.stranded;
    let cleanup = abandon_children(children, reap, stranded, instance);
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => PluginError::Spawn(format!("{error}; cleanup: {cleanup}")),
    }
}

/// A start that failed after its launch: the whole group is killed on the
/// sentinel's authority while it is still held, through the same teardown as
/// a running bridge, then whatever did not end is abandoned to stranded
/// custody. The record is forgotten only once the group is confirmed gone.
fn abandon_launched(
    containment: &AggregateContainment,
    instance: u64,
    leader: Child,
    sentinel: Child,
    reap: Duration,
    record: &Journalled,
    error: PluginError,
) -> PluginError {
    let group = sentinel.id();
    let leader_pid = leader.id();
    let custody: Custody = Arc::new(Mutex::new(Some(sentinel)));
    let mut leader = Some(leader);
    let mut children = Vec::new();
    let stopped = kill_tree(
        group,
        leader_pid,
        0,
        &Tracked::new(),
        &custody,
        reap,
        Some(record),
    );
    // A leader that survives its bound stays in `leader` for stranded custody.
    let released = release(&mut leader, reap, "abandoned bridge leader");
    children.extend(leader.take());
    if let Ok(mut held) = custody.lock() {
        children.extend(held.take());
    }
    let error = match both(stopped, released) {
        Ok(()) => error,
        Err(cleanup) => PluginError::Spawn(format!("{error}; teardown: {cleanup}")),
    };
    // The record stays unless `kill_tree` already confirmed the group gone.
    abandon(containment, instance, children, reap, error)
}

/// `abandon` for a start that already built its control: the tree is killed
/// through its custody first (which removes its record once the sentinel is
/// reaped), then whatever remains is abandoned.
fn abandon_control(
    containment: &AggregateContainment,
    instance: u64,
    control: &ProcessControl,
    error: PluginError,
) -> PluginError {
    let mut children = Vec::new();
    let stopped = control.stop_containment();
    if let Ok(mut held) = control.child.lock() {
        children.extend(held.take());
    }
    let ProcessContainment::Watchdog { custody, .. } = &control.containment;
    if let Ok(mut held) = custody.lock() {
        children.extend(held.take());
    }
    let error = match stopped {
        Ok(()) => error,
        Err(stop) => PluginError::Spawn(format!("{error}; teardown: {stop}")),
    };
    abandon(containment, instance, children, control.reap, error)
}

/// Spawn a sentinel: a new group leader whose argv[0] is `tag`. INT_MAX
/// seconds is the longest interval Apple's sleep accepts; the nonce rides in
/// argv[0], never as an operand sleep would parse.
pub(super) fn spawn_sentinel(tag: &str) -> Result<Child, PluginError> {
    Command::new("/bin/sleep")
        .arg0(tag)
        .arg("2147483647")
        .env_clear()
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| PluginError::Spawn(format!("plugin group sentinel refused: {error}")))
}

/// Spawn `command` into the sentinel's `group`, with only the required
/// environment: it never inherits the service's credential-bearing one.
pub(super) fn spawn_in_group(mut command: Command, group: u32) -> Result<Child, PluginError> {
    let group = signal::bridge_id(group, own())
        .map_err(|refusal| PluginError::Spawn(format!("refusing the bridge group: {refusal}")))?;
    command.env_clear().process_group(group);
    for (name, value) in super::super::validation::required_environment() {
        command.env(name, value);
    }
    command
        .spawn()
        .map_err(|error| PluginError::Spawn(error.to_string()))
}

/// The journalled launch's spawns (`journal::launch`): a sentinel leading a
/// new group with the nonce in its argv[0], then the bridge in that group.
struct Spawner<'a> {
    access: &'a super::super::isolation::ProcessAccess,
    stranded: &'a Stranded<Child>,
    instance: u64,
    reap: Duration,
}

impl journal::Launcher for Spawner<'_> {
    type Sentinel = Child;
    type Bridge = Child;

    fn sentinel(&mut self, tag: &str) -> Result<(Child, Recorded), journal::NotLaunched> {
        let child = spawn_sentinel(tag).map_err(|error| journal::NotLaunched {
            error,
            nothing_left: true,
        })?;
        let pid = child.id();
        match signal::Probe::observe(&Live, pid) {
            Seen::Alive { started, .. } => Ok((child, Recorded { pid, started })),
            _ => {
                let error = PluginError::Spawn(
                    "the group sentinel's identity could not be read".to_owned(),
                );
                // Only a confirmed reap leaves nothing behind.
                Err(
                    match abandon_children(vec![child], self.reap, self.stranded, self.instance) {
                        Ok(()) => journal::NotLaunched {
                            error,
                            nothing_left: true,
                        },
                        Err(cleanup) => journal::NotLaunched {
                            error: PluginError::Spawn(format!("{error}; cleanup: {cleanup}")),
                            nothing_left: false,
                        },
                    },
                )
            }
        }
    }

    fn bridge(&mut self, group: u32) -> Result<Child, PluginError> {
        let mut command = grapt_sandbox::Seatbelt::piped_command(&self.access.request)
            .map_err(|error| PluginError::Spawn(format!("plugin_sandbox_unavailable: {error}")))?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        spawn_in_group(command, group)
    }

    fn abandon(&mut self, sentinel: Child) -> Result<(), PluginError> {
        abandon_children(vec![sentinel], self.reap, self.stranded, self.instance)
    }
}

pub(super) fn spawn_bridge(
    _runtime: &VerifiedRuntime,
    containment: &AggregateContainment,
    limits: BridgeLimits,
    instance: u64,
    access: &super::super::isolation::ProcessAccess,
) -> Result<Spawned, PluginError> {
    if limits.tasks == 0 || limits.cpu_percent == 0 {
        return Err(PluginError::Runtime(
            "plugin macOS containment bounds must be nonzero".to_owned(),
        ));
    }
    let supervision = &containment.watchdog;
    let reap = Duration::from_millis(limits.reap_millis);
    let intent = supervision.intent()?;
    let mut spawner = Spawner {
        access,
        stranded: &supervision.stranded,
        instance,
        reap,
    };
    let (sentinel, mut child, record) =
        journal::launch(&supervision.journal, &mut spawner, &LiveCensus, intent)?;
    let record = Arc::new(Journalled {
        journal: Arc::clone(&supervision.journal),
        record,
    });
    let group = sentinel.id();
    let leader = child.id();
    // The leader's start bounds every process the teardown may signal.
    let Seen::Alive { started: floor, .. } = signal::Probe::observe(&Live, leader) else {
        let error = PluginError::Spawn("the bridge leader's identity could not be read".to_owned());
        return Err(abandon_launched(
            containment,
            instance,
            child,
            sentinel,
            reap,
            &record,
            error,
        ));
    };
    let Some((input, output)) = child.stdin.take().zip(child.stdout.take()) else {
        let error = PluginError::Spawn("bridge pipes were not established".to_owned());
        return Err(abandon_launched(
            containment,
            instance,
            child,
            sentinel,
            reap,
            &record,
            error,
        ));
    };
    let control = ProcessControl {
        child: Arc::new(Mutex::new(Some(child))),
        violation: Arc::new(Mutex::new(None)),
        exit_observed: Arc::new(AtomicBool::new(false)),
        containment: ProcessContainment::Watchdog {
            group,
            leader,
            floor,
            tracked: Arc::new(Mutex::new(Tracked::new())),
            custody: Arc::new(Mutex::new(Some(sentinel))),
            record: Some(record),
        },
        cleanup: Duration::from_millis(limits.cleanup_millis),
        reap,
    };
    if let Err(error) = supervision.watch(control.clone(), group, leader, &limits) {
        return Err(abandon_control(containment, instance, &control, error));
    }
    Ok(Spawned {
        input: BridgeInput::Direct(input),
        output: Box::new(output),
        control,
    })
}

struct Watched {
    control: ProcessControl,
    watch: Watch,
    sample: Duration,
}

#[derive(Default)]
struct Registry {
    watched: Mutex<Vec<Watched>>,
    stopped: AtomicBool,
}

/// One watchdog thread for every bridge of a supervisor, so the aggregate
/// bounds are judged over the same snapshot as each bridge's own.
pub(in crate::supervisor) struct Supervision {
    registry: Arc<Registry>,
    /// The launch journal a successor reclaims from (SC-MAC-02b).
    journal: Arc<Journal>,
    recovery: super::super::RecoveryJournal,
    uid: u32,
    owner: Recorded,
    /// Children failed starts could not reap, with their admissions.
    pub(in crate::supervisor) stranded: Arc<Stranded<Child>>,
    aggregate: Arc<Aggregate>,
    cores: f64,
}

impl core::fmt::Debug for Supervision {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Supervision")
            .finish_non_exhaustive()
    }
}

/// What one tick decided, acted on after the registry is released.
enum Action {
    Stop(ProcessControl),
    Terminate(ProcessControl, Verdict),
}

impl Supervision {
    /// Open the journal, reclaim a crashed predecessor's launches within
    /// their bounds, then start the watchdog.
    pub(in crate::supervisor) fn establish(
        budget: SupervisorBudget,
        recovery: &super::super::RecoveryJournal,
        reap: Duration,
    ) -> Result<Self, PluginError> {
        let uid = getuid().as_raw();
        let journal = Journal::open(&recovery.root, uid)?;
        let Seen::Alive { started, .. } = signal::Probe::observe(&Live, std::process::id()) else {
            return Err(PluginError::Runtime(
                "the supervisor's own identity could not be read".to_owned(),
            ));
        };
        let limit = budget.processes.saturating_mul(4).max(16);
        let report = journal::reclaim(&journal, &LiveCensus, &mut Kernel, own(), limit, reap)?;
        if report.busy || !report.unverified.is_empty() {
            grapt_diagnostics::report(format!(
                "grapt-plugin: plugin recovery left launches unreclaimed (busy: {}): {}",
                report.busy,
                report.unverified.join("; ")
            ));
        }
        let supervision = Self {
            registry: Arc::new(Registry::default()),
            journal: Arc::new(journal),
            recovery: recovery.clone(),
            uid,
            owner: Recorded {
                pid: std::process::id(),
                started,
            },
            stranded: Arc::new(Stranded::default()),
            aggregate: Arc::new(Aggregate::new(&budget)),
            cores: f64::from(budget.cpu_percent) / 100.0,
        };
        let registry = Arc::clone(&supervision.registry);
        let aggregate = Arc::clone(&supervision.aggregate);
        let stranded = Arc::clone(&supervision.stranded);
        thread::Builder::new()
            .name("plugin-watchdog".to_owned())
            .spawn(move || run(&registry, &aggregate, &stranded))
            .map_err(|error| PluginError::Spawn(error.to_string()))?;
        Ok(supervision)
    }

    /// A new launch's intent: a fresh nonce, this supervisor, now.
    fn intent(&self) -> Result<Record, PluginError> {
        let nonce = self.recovery.ids.mint().as_str().to_owned();
        if !journal::valid_nonce(&nonce) {
            return Err(PluginError::Runtime(
                "the injected identity source minted an unusable launch nonce".to_owned(),
            ));
        }
        Ok(Record {
            version: journal::RECORD_VERSION,
            nonce,
            uid: self.uid,
            owner: self.owner,
            intent_millis: self.recovery.clock.now().as_millis(),
            sentinel: None,
        })
    }

    /// Watch a bridge whose group its sentinel (`group`) leads.
    fn watch(
        &self,
        control: ProcessControl,
        group: u32,
        leader: u32,
        limits: &BridgeLimits,
    ) -> Result<(), PluginError> {
        let mut watched = self
            .registry
            .watched
            .lock()
            .map_err(|_| PluginError::Poisoned)?;
        watched.push(Watched {
            control,
            watch: Watch::new(leader, limits, self.cores)
                .grouped(group)
                .pinned_by(group),
            sample: Duration::from_millis(limits.sample_millis.max(10)),
        });
        Ok(())
    }
}

impl Drop for Supervision {
    fn drop(&mut self) {
        self.registry.stopped.store(true, Ordering::Release);
        if let Ok(mut watched) = self.registry.watched.lock() {
            for watched in watched.drain(..) {
                let _ = watched.control.stop_containment();
            }
        }
        // A bounded last chance for failed starts' children before the
        // supervisor that owns them goes away.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while self.stranded.held() > 0 && tokio::time::Instant::now() < deadline {
            self.stranded.sweep();
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn run(registry: &Registry, aggregate: &Aggregate, stranded: &Stranded<Child>) {
    let mut system = System::new();
    loop {
        let sample = registry
            .watched
            .lock()
            .ok()
            .and_then(|watched| watched.iter().map(|watched| watched.sample).min())
            .unwrap_or(Duration::from_millis(100));
        thread::sleep(sample);
        if registry.stopped.load(Ordering::Acquire) {
            return;
        }
        stranded.sweep();
        let actions = match registry.watched.lock() {
            Ok(mut watched) => tick(&mut watched, &snapshot(&mut system), aggregate),
            Err(_) => return,
        };
        for action in actions {
            match action {
                Action::Stop(control) => {
                    let _ = control.stop_containment();
                }
                Action::Terminate(control, Verdict::Resource(reason)) => {
                    let _ = control.terminate_for(ViolationKind::Resource, reason);
                }
                Action::Terminate(control, Verdict::Other(reason)) => {
                    let _ = control.terminate_for(ViolationKind::Other, reason);
                }
            }
        }
    }
}

/// Judge every live bridge and the aggregate over one snapshot. A bridge
/// whose leader exited leaves the registry with its leftovers killed.
fn tick(watched: &mut Vec<Watched>, table: &Table, aggregate: &Aggregate) -> Vec<Action> {
    let now = tokio::time::Instant::now();
    let mut actions = Vec::new();
    let mut trees = Vec::new();
    watched.retain_mut(|entry| {
        if entry.control.has_exited().unwrap_or(true) {
            actions.push(Action::Stop(entry.control.clone()));
            return false;
        }
        let judgment = entry.watch.judge(table, now);
        entry.control.track(&entry.watch.tracked);
        match judgment.verdict {
            Some(verdict) => {
                actions.push(Action::Terminate(entry.control.clone(), verdict));
                false
            }
            None => {
                trees.push((entry.watch.leader, judgment.rss, judgment.tasks));
                true
            }
        }
    });
    if let Some((leader, verdict)) = aggregate.judge(&trees) {
        if let Some(index) = watched
            .iter()
            .position(|entry| entry.watch.leader == leader)
        {
            let entry = watched.remove(index);
            actions.push(Action::Terminate(entry.control, verdict));
        }
    }
    actions
}

#[cfg(test)]
#[path = "macos_tests.rs"]
mod tests;
