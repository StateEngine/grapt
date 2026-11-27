#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteVmBudget(NonZeroU64);

impl SqliteVmBudget {
    pub fn new(steps: u64) -> Option<Self> {
        NonZeroU64::new(steps).map(Self)
    }

    pub fn production() -> Self {
        match Self::new(MAX_SQLITE_VM_STEPS) {
            Some(value) => value,
            None => unreachable!("the production SQLite VM-step budget is positive"),
        }
    }

    pub const fn steps(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn open_verification() -> Self {
        match Self::new(MAX_OPEN_VERIFY_VM_STEPS) {
            Some(value) => value,
            None => unreachable!("the open verification budget is positive"),
        }
    }
}

/// Installed progress handler. Exclusive ownership prevents a sibling from
/// replacing it; finishing or dropping the guard removes it.
#[derive(Debug)]
pub struct VmBudgetGuard<'conn> {
    conn: &'conn mut Connection,
    exhausted: std::sync::Arc<std::sync::atomic::AtomicBool>,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    installed: bool,
}

impl VmBudgetGuard<'_> {
    pub fn exhausted(&self) -> bool {
        self.exhausted.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn connection(&self) -> &Connection {
        self.conn
    }

    pub fn finish(mut self) -> rusqlite::Result<()> {
        self.conn.progress_handler(0, None::<fn() -> bool>)?;
        self.installed = false;
        Ok(())
    }

    pub(crate) fn begin_immediate(&mut self) -> rusqlite::Result<Transaction<'_>> {
        self.conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
    }

    pub(crate) fn stamp_and_prepare(
        &mut self,
        engine_version: &str,
        prepare: impl FnOnce(&Transaction<'_>) -> rusqlite::Result<u64>,
    ) -> Result<crate::epoch::EpochOutcome, crate::epoch::EpochError> {
        crate::epoch::stamp_and_prepare(self.conn, engine_version, prepare)
    }

    /// Register before BEGIN; spend the budget only around projection verification.
    pub(crate) fn suspend(&mut self) -> DeferredVmBudget {
        self.active
            .store(false, std::sync::atomic::Ordering::SeqCst);
        DeferredVmBudget {
            active: self.active.clone(),
            exhausted: self.exhausted.clone(),
        }
    }
}

impl Drop for VmBudgetGuard<'_> {
    fn drop(&mut self) {
        if self.installed {
            let _ = self.conn.progress_handler(0, None::<fn() -> bool>);
        }
    }
}

pub fn install_vm_budget(
    conn: &mut Connection,
    budget: SqliteVmBudget,
) -> rusqlite::Result<VmBudgetGuard<'_>> {
    let quantum_u64 = budget.steps().min(1_000);
    let quantum = i32::try_from(quantum_u64).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let consumed = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let exhausted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let consumed_for_handler = std::sync::Arc::clone(&consumed);
    let exhausted_for_handler = std::sync::Arc::clone(&exhausted);
    let active_for_handler = std::sync::Arc::clone(&active);
    let steps = budget.steps();
    conn.progress_handler(
        quantum,
        Some(move || {
            if !active_for_handler.load(std::sync::atomic::Ordering::SeqCst) {
                return false;
            }
            let total = consumed_for_handler
                .fetch_add(quantum_u64, std::sync::atomic::Ordering::SeqCst)
                + quantum_u64;
            if total >= steps {
                exhausted_for_handler.store(true, std::sync::atomic::Ordering::SeqCst);
                true
            } else {
                false
            }
        }),
    )?;
    Ok(VmBudgetGuard {
        conn,
        exhausted,
        active,
        installed: true,
    })
}

/// Registration stays with the connection owner; only budget activation crosses
/// the immutable transaction borrow used by schema preparation.
#[derive(Debug)]
pub(crate) struct DeferredVmBudget {
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
    exhausted: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl DeferredVmBudget {
    pub(crate) fn enter(&self) -> rusqlite::Result<VmBudgetScope<'_>> {
        if self.active.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok(VmBudgetScope(self))
    }

    fn exhausted(&self) -> bool {
        self.exhausted.load(std::sync::atomic::Ordering::SeqCst)
    }
}

pub(crate) struct VmBudgetScope<'a>(&'a DeferredVmBudget);

impl Drop for VmBudgetScope<'_> {
    fn drop(&mut self) {
        self.0
            .active
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}
