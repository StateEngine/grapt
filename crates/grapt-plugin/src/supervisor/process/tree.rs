//! What macOS's watchdog containment decides (SC-MAC-02), kept free of any
//! operating-system call so every platform's tests prove it.
//!
//! macOS has neither a cgroup nor a Job Object: nothing in its kernel bounds
//! a process tree's memory, and `RLIMIT_AS` is not enforced. Each bridge
//! leads its own process group, and its tree is that group plus every
//! descendant by parent PID, read from one process-table snapshot per sample.
//! The limits are the ones Linux and Windows enforce, applied by killing the
//! tree. A process that leaves both the group and the parent chain before any
//! sample sees it is not contained; one that was seen and then left is
//! reported as an escape and killed by its recorded identity.

use std::collections::{BTreeMap, VecDeque};
use tokio::time::Instant;

use super::signal::Identity;
use crate::protocol::{BridgeLimits, SupervisorBudget};

/// The honest name of this containment, carried by every macOS verdict.
pub(super) const WATCHDOG_CONTAINMENT: &str = "macOS containment is watchdog-enforced, not kernel-enforced: memory is sampled resident memory that the watchdog kills, and a process that leaves the bridge's process group and parent chain is not contained";

/// One process in a snapshot of the process table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) pid: u32,
    pub(super) parent: Option<u32>,
    pub(super) group: Option<u32>,
    /// Start time: a live process is told from a reused PID by it.
    pub(super) started: u64,
    pub(super) rss: u64,
    pub(super) cpu_millis: u64,
    /// Whether the platform actually read this process's memory and CPU. A
    /// failed read reports zeros, never an error, and a live process always
    /// holds resident memory; only a zombie legitimately reads zero.
    pub(super) readable: bool,
}

pub(super) type Table = BTreeMap<u32, Entry>;

/// Processes by PID with the identity each was last seen with.
pub(super) type Tracked = BTreeMap<u32, Identity>;

/// `entry`'s identity in `table`, with its parent's.
pub(super) fn identity(table: &Table, entry: &Entry) -> Identity {
    Identity {
        pid: entry.pid,
        started: entry.started,
        parent: entry.parent,
        parent_started: entry
            .parent
            .and_then(|parent| table.get(&parent))
            .map(|parent| parent.started),
    }
}

/// The tree `leader` owns in `table`: its process group plus every
/// descendant by parent PID, so a `setsid` child stays in while its chain lives.
#[cfg(test)]
pub(super) fn members(table: &Table, leader: u32) -> Vec<Entry> {
    members_of(table, leader, leader)
}

/// The tree of a bridge `leader` whose process group is `group`, which a
/// journalled launch's sentinel leads (SC-MAC-02b): the group plus every
/// descendant of the leader.
pub(super) fn members_of(table: &Table, group: u32, leader: u32) -> Vec<Entry> {
    table
        .values()
        .filter(|entry| entry.group == Some(group) || descends(table, entry.pid, leader))
        .copied()
        .collect()
}

fn descends(table: &Table, pid: u32, leader: u32) -> bool {
    let mut current = Some(pid);
    // Bounded by the table, so a cyclic snapshot cannot spin.
    for _ in 0..=table.len() {
        match current {
            Some(pid) if pid == leader => return true,
            Some(pid) => current = table.get(&pid).and_then(|entry| entry.parent),
            None => return false,
        }
    }
    false
}

/// Tracked processes still alive under the same start time that the tree no
/// longer holds: they left the group and the parent chain.
pub(super) fn escaped(tracked: &Tracked, table: &Table, members: &[Entry]) -> Tracked {
    tracked
        .iter()
        .filter(|(pid, identity)| {
            table
                .get(pid)
                .is_some_and(|entry| entry.started == identity.started)
                && !members.iter().any(|member| member.pid == **pid)
        })
        .map(|(pid, identity)| (*pid, *identity))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    Resource(String),
    Other(String),
}

/// What one sample decided for one bridge.
#[derive(Debug, Default)]
pub(super) struct Judgment {
    pub(super) verdict: Option<Verdict>,
    pub(super) rss: u64,
    pub(super) tasks: usize,
}

/// A CPU budget judged over accumulated CPU time across a sampling window,
/// as on Windows: a quota is a rate, so point samples are not evidence.
#[derive(Debug)]
struct CpuWindow {
    cores: f64,
    samples: usize,
    accounting: VecDeque<(Instant, u64)>,
}

impl CpuWindow {
    fn new(cores: f64, samples: u16) -> Self {
        let samples = usize::from(samples.max(1));
        Self {
            cores,
            samples,
            accounting: VecDeque::with_capacity(samples + 1),
        }
    }

    /// Whether the window's CPU use reached the quota; `cpu_millis` is the
    /// tree's monotone charge, so members that exit keep what they used.
    fn exhausted(&mut self, now: Instant, cpu_millis: u64) -> bool {
        self.accounting.push_back((now, cpu_millis));
        if self.accounting.len() > self.samples + 1 {
            self.accounting.pop_front();
        }
        if self.accounting.len() <= self.samples {
            return false;
        }
        let (Some((first_at, first)), Some((last_at, last))) =
            (self.accounting.front(), self.accounting.back())
        else {
            return false;
        };
        let wall = last_at.duration_since(*first_at).as_secs_f64();
        let used = last.saturating_sub(*first) as f64 / 1000.0;
        wall > 0.0 && used / wall >= self.cores * 0.85
    }
}

/// One bridge's watch: its limits, its CPU window and the processes it was
/// last seen to own.
#[derive(Debug)]
pub(super) struct Watch {
    pub(super) leader: u32,
    rss_bytes: u64,
    tasks: u16,
    cpu_percent: u16,
    cpu_samples: u16,
    cpu: CpuWindow,
    /// CPU charged to the tree so far: each member's growth since it was
    /// last seen, so a member that exits keeps its charge.
    charged: u64,
    /// Each member's accumulated CPU as last seen, by PID and start time.
    seen_cpu: BTreeMap<(u32, u64), u64>,
    pub(super) tracked: Tracked,
    missing: u8,
    /// The supervisor's own process pinned in the group (`macos::pin_group`):
    /// it holds the group ID, and is not the bridge's to charge or count.
    sentinel: Option<u32>,
    /// The bridge's process group: the leader's own, or its sentinel's.
    group: u32,
}

impl Watch {
    pub(super) fn new(leader: u32, limits: &BridgeLimits, aggregate_cores: f64) -> Self {
        let cores = (f64::from(limits.cpu_percent) / 100.0).min(aggregate_cores);
        Self {
            leader,
            rss_bytes: limits.rss_bytes,
            tasks: limits.tasks,
            cpu_percent: limits.cpu_percent,
            cpu_samples: limits.cpu_violation_samples,
            cpu: CpuWindow::new(cores, limits.cpu_violation_samples),
            charged: 0,
            seen_cpu: BTreeMap::new(),
            tracked: Tracked::new(),
            missing: 0,
            sentinel: None,
            group: leader,
        }
    }

    /// The bridge's group, when a sentinel leads it rather than the leader.
    pub(super) fn grouped(mut self, group: u32) -> Self {
        self.group = group;
        self
    }

    pub(super) fn pinned_by(mut self, sentinel: u32) -> Self {
        self.sentinel = Some(sentinel);
        self
    }

    /// A sample the watchdog could not read. Three in a row fail closed.
    fn unreadable(&mut self, what: &str) -> Judgment {
        self.missing = self.missing.saturating_add(1);
        Judgment {
            verdict: (self.missing >= 3).then(|| {
                Verdict::Other(format!(
                    "plugin resource telemetry {what} for 3 samples; the owned bridge was terminated fail-closed. {WATCHDOG_CONTAINMENT}"
                ))
            }),
            ..Judgment::default()
        }
    }

    /// Judge one sample of a bridge whose leader has not exited.
    pub(super) fn judge(&mut self, table: &Table, now: Instant) -> Judgment {
        if !table.contains_key(&self.leader) {
            return self.unreadable("lost the bridge leader");
        }
        let mut members = members_of(table, self.group, self.leader);
        members.retain(|entry| Some(entry.pid) != self.sentinel);
        let unread = members.iter().filter(|entry| !entry.readable).count();
        if unread > 0 {
            return self.unreadable(&format!(
                "was unreadable for {unread} live bridge process(es)"
            ));
        }
        self.missing = 0;
        let escaped = escaped(&self.tracked, table, &members);
        // Escapees stay tracked so the teardown can still kill them.
        self.tracked = members
            .iter()
            .map(|entry| (entry.pid, identity(table, entry)))
            .chain(escaped.iter().map(|(pid, identity)| (*pid, *identity)))
            .collect();
        let rss = members.iter().map(|entry| entry.rss).sum::<u64>();
        let seen = std::mem::take(&mut self.seen_cpu);
        for entry in &members {
            let identity = (entry.pid, entry.started);
            let before = seen.get(&identity).copied().unwrap_or(0);
            self.charged = self
                .charged
                .saturating_add(entry.cpu_millis.saturating_sub(before));
            self.seen_cpu.insert(identity, entry.cpu_millis);
        }
        let tasks = members.len();
        let verdict = if !escaped.is_empty() {
            Some(Verdict::Other(format!(
                "{} plugin bridge process(es) left the bridge's process group and parent chain and were killed with the bridge. {WATCHDOG_CONTAINMENT}",
                escaped.len()
            )))
        } else if rss > self.rss_bytes {
            Some(Verdict::Resource(format!(
                "plugin bridge RSS {rss} exceeded the observed {}-byte termination threshold. {WATCHDOG_CONTAINMENT}",
                self.rss_bytes
            )))
        } else if tasks > usize::from(self.tasks) {
            Some(Verdict::Resource(format!(
                "plugin bridge tree held {tasks} processes against its {}-process bound. {WATCHDOG_CONTAINMENT}",
                self.tasks
            )))
        } else if self.cpu.exhausted(now, self.charged) {
            Some(Verdict::Resource(format!(
                "plugin bridge CPU exhausted its {}% CPU quota across {} samples; the owned process tree was terminated. {WATCHDOG_CONTAINMENT}",
                self.cpu_percent, self.cpu_samples
            )))
        } else {
            None
        };
        Judgment {
            verdict,
            rss,
            tasks,
        }
    }
}

/// The supervisor's aggregate memory and process bounds over every live
/// tree. Linux's slice and Windows' Job enforce these in the kernel; here
/// the watchdog terminates the largest tree, as an out-of-memory kill would.
/// The aggregate CPU budget is not enforced on macOS: nothing throttles it,
/// and each bridge's own CPU quota still holds.
#[derive(Debug)]
pub(super) struct Aggregate {
    rss_bytes: u64,
    tasks: u16,
}

impl Aggregate {
    pub(super) fn new(budget: &SupervisorBudget) -> Self {
        Self {
            rss_bytes: budget.rss_bytes,
            tasks: budget.tasks,
        }
    }

    /// The leader to terminate, from each live tree's `(leader, rss, tasks)`.
    pub(super) fn judge(&self, trees: &[(u32, u64, usize)]) -> Option<(u32, Verdict)> {
        let rss = trees.iter().map(|tree| tree.1).sum::<u64>();
        let tasks = trees.iter().map(|tree| tree.2).sum::<usize>();
        if rss > self.rss_bytes {
            let (leader, ..) = trees.iter().max_by_key(|tree| tree.1)?;
            return Some((*leader, Verdict::Resource(format!(
                "plugin bridges together held {rss} bytes RSS against the supervisor's {}-byte aggregate threshold; the largest tree was terminated. {WATCHDOG_CONTAINMENT}",
                self.rss_bytes
            ))));
        }
        if tasks > usize::from(self.tasks) {
            let (leader, ..) = trees.iter().max_by_key(|tree| tree.2)?;
            return Some((*leader, Verdict::Resource(format!(
                "plugin bridges together held {tasks} processes against the supervisor's {}-process aggregate bound; the largest tree was terminated. {WATCHDOG_CONTAINMENT}",
                self.tasks
            ))));
        }
        None
    }
}

#[cfg(test)]
#[path = "tree_tests.rs"]
mod tests;
