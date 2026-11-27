//! Supervisor-wide process, startup, memory, CPU, and task budgets.

use std::sync::atomic::AtomicUsize;
use std::sync::Condvar;

use super::*;

/// Counts every live bridge process of one supervisor, including a candidate
/// generation starting beside the generation it may replace.
#[derive(Debug)]
pub(super) struct ProcessBudget {
    live: Arc<AtomicUsize>,
    limit: usize,
    startup: Arc<(Mutex<usize>, Condvar)>,
    startup_limit: usize,
}

#[derive(Debug)]
pub(super) struct ProcessBudgetSlot {
    live: Arc<AtomicUsize>,
}

impl Drop for ProcessBudgetSlot {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) struct StartupPermit {
    startup: Arc<(Mutex<usize>, Condvar)>,
}

impl Drop for StartupPermit {
    fn drop(&mut self) {
        let (count, released) = &*self.startup;
        if let Ok(mut count) = count.lock() {
            *count = count.saturating_sub(1);
        }
        released.notify_one();
    }
}

impl ProcessBudget {
    pub(super) fn new(budget: SupervisorBudget) -> Result<Self, PluginError> {
        if budget.processes == 0
            || budget.startup_concurrency == 0
            || budget.rss_bytes == 0
            || budget.cpu_percent == 0
            || budget.tasks == 0
        {
            return Err(PluginError::Runtime(
                "plugin supervisor budget bounds must be nonzero".to_owned(),
            ));
        }
        if budget.processes < 2 {
            return Err(PluginError::Runtime(
                "plugin supervisor process budget must admit a candidate beside its published generation (at least 2 processes)".to_owned(),
            ));
        }
        Ok(Self {
            live: Arc::new(AtomicUsize::new(0)),
            limit: budget.processes,
            startup: Arc::new((Mutex::new(0), Condvar::new())),
            startup_limit: budget.startup_concurrency,
        })
    }

    /// The most plugins one generation may hold. A reload starts every
    /// candidate process beside the published generation, which retires only
    /// after publication, so both must fit inside the process budget at once.
    pub(super) fn reload_safe_plugins(&self) -> usize {
        self.limit / 2
    }

    pub(super) fn limit(&self) -> usize {
        self.limit
    }

    pub(super) fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Reserve `count` process slots or refuse the whole candidate.
    pub(super) fn reserve(&self, count: usize) -> Result<Vec<ProcessBudgetSlot>, PluginError> {
        let reserved = self
            .live
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |live| {
                live.checked_add(count).filter(|total| *total <= self.limit)
            })
            .map_err(|live| {
                PluginError::Registration(format!(
                    "plugin process budget exhausted: {live} live bridge processes plus {count} requested exceeds the supervisor bound of {}; a retired generation is still being reaped, so retry the refresh once its processes exit",
                    self.limit
                ))
            });
        reserved?;
        Ok((0..count)
            .map(|_| ProcessBudgetSlot {
                live: Arc::clone(&self.live),
            })
            .collect())
    }

    pub(super) fn startup_permit(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<StartupPermit, PluginError> {
        let (count, released) = &*self.startup;
        let mut held = count.lock().map_err(|_| PluginError::Poisoned)?;
        while *held >= self.startup_limit {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .ok_or(PluginError::Timeout("plugin startup admission"))?;
            held = released
                .wait_timeout(held, remaining)
                .map_err(|_| PluginError::Poisoned)?
                .0;
        }
        *held += 1;
        Ok(StartupPermit {
            startup: Arc::clone(&self.startup),
        })
    }
}

/// Aggregate containment for every bridge process of one supervisor: hard,
/// kernel-enforced on Linux and Windows; watchdog-enforced on macOS.
pub(super) struct AggregateContainment {
    #[cfg(target_os = "linux")]
    pub(super) slice: String,
    #[cfg(windows)]
    pub(super) group: processkit::ProcessGroup,
    #[cfg(windows)]
    pub(super) cpu_cores: f64,
    #[cfg(target_os = "macos")]
    pub(super) watchdog: super::process::macos::Supervision,
}

impl core::fmt::Debug for AggregateContainment {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("AggregateContainment")
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
static NEXT_SLICE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl AggregateContainment {
    #[cfg(target_os = "linux")]
    pub(super) fn establish(
        budget: SupervisorBudget,
        _recovery: &super::RecoveryJournal,
        _reap: Duration,
    ) -> Result<Self, PluginError> {
        let sequence = NEXT_SLICE_ID
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| PluginError::Runtime("plugin slice identity overflowed".to_owned()))?;
        let slice = format!("secode_plugins_{}_{sequence}.slice", std::process::id());
        super::process::linux::configure_slice(&slice, budget)?;
        Ok(Self { slice })
    }

    #[cfg(windows)]
    pub(super) fn establish(
        budget: SupervisorBudget,
        _recovery: &super::RecoveryJournal,
        _reap: Duration,
    ) -> Result<Self, PluginError> {
        let processes = u32::from(budget.tasks);
        let cpu_cores = f64::from(budget.cpu_percent) / 100.0;
        let group = processkit::ProcessGroup::with_options(
            processkit::ProcessGroupOptions::default()
                .max_memory(budget.rss_bytes)
                .max_processes(processes)
                .cpu_quota(cpu_cores),
        )
        .map_err(|error| PluginError::Spawn(error.to_string()))?;
        Ok(Self { group, cpu_cores })
    }

    /// macOS has no kernel boundary for a process tree: the aggregate is
    /// one watchdog over every bridge (see `process::tree`).
    /// macOS also reclaims a crashed predecessor's launches first, from the
    /// recovery journal (SC-MAC-02b).
    #[cfg(target_os = "macos")]
    pub(super) fn establish(
        budget: SupervisorBudget,
        recovery: &super::RecoveryJournal,
        reap: Duration,
    ) -> Result<Self, PluginError> {
        Ok(Self {
            watchdog: super::process::macos::Supervision::establish(budget, recovery, reap)?,
        })
    }

    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    pub(super) fn establish(
        _budget: SupervisorBudget,
        _recovery: &super::RecoveryJournal,
        _reap: Duration,
    ) -> Result<Self, PluginError> {
        Err(PluginError::Runtime(
            process::UNSUPPORTED_PLATFORM.to_owned(),
        ))
    }
}

impl AggregateContainment {
    /// A start failed: its admission stays charged while any child it left
    /// unreaped is still owned (macOS); otherwise the admission is released.
    pub(super) fn settle_failed_start(&self, instance: u64, admission: ProcessBudgetSlot) {
        #[cfg(target_os = "macos")]
        let admission = self.watchdog.stranded.charge(instance, admission);
        #[cfg(not(target_os = "macos"))]
        let _ = instance;
        drop(admission);
    }
}

impl Drop for AggregateContainment {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        super::process::linux::stop_slice(&self.slice);
        #[cfg(windows)]
        {
            let _ = self.group.kill_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::DEFAULT_SUPERVISOR_BUDGET;

    #[test]
    fn a_reservation_beyond_the_process_budget_is_refused_whole() {
        let budget = ProcessBudget::new(SupervisorBudget {
            processes: 2,
            ..DEFAULT_SUPERVISOR_BUDGET
        })
        .unwrap_or_else(|error| panic!("budget construction failed: {error}"));
        let held = budget
            .reserve(2)
            .unwrap_or_else(|error| panic!("an in-bound reservation was refused: {error}"));
        let refused = budget.reserve(1);
        assert!(
            matches!(&refused, Err(PluginError::Registration(message)) if message.contains("plugin process budget exhausted")),
            "a reservation beyond the process budget was admitted: {refused:?}"
        );
        assert_eq!(budget.live(), 2);
        drop(held);
        assert_eq!(budget.live(), 0);
        assert!(ProcessBudget::new(SupervisorBudget {
            processes: 1,
            ..DEFAULT_SUPERVISOR_BUDGET
        })
        .is_err());
    }
}
