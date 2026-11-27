//! Table-gated commit faults, reset on full transaction rollback.
//! Savepoint rollback and statement-level abort do not reset pending writes.
use super::EventStore;
use rusqlite::hooks::Action;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Dropping the guard disables every captured callback and clears pending writes.
#[derive(Debug)]
pub struct CommitFault {
    active: Arc<AtomicBool>,
    changed: Arc<AtomicBool>,
}

impl CommitFault {
    pub fn install(
        store: &mut EventStore,
        action: Action,
        table: &'static str,
        mut allowed: usize,
    ) -> rusqlite::Result<Self> {
        let fault = Self {
            active: Arc::new(AtomicBool::new(true)),
            changed: Arc::new(AtomicBool::new(false)),
        };
        let active = fault.active.clone();
        let changed = fault.changed.clone();
        store.update_hook(Some(move |seen, database: &str, name: &str, _row| {
            if active.load(Ordering::SeqCst)
                && seen == action
                && database == "main"
                && name == table
            {
                changed.store(true, Ordering::SeqCst);
            }
        }))?;
        let changed = fault.changed.clone();
        store.rollback_hook(Some(move || changed.store(false, Ordering::SeqCst)))?;
        let active = fault.active.clone();
        let changed = fault.changed.clone();
        store.commit_hook(Some(move || {
            if !changed.swap(false, Ordering::SeqCst) || !active.load(Ordering::SeqCst) {
                false
            } else if allowed == 0 {
                true
            } else {
                allowed -= 1;
                false
            }
        }))?;
        Ok(fault)
    }
}

impl Drop for CommitFault {
    fn drop(&mut self) {
        self.active.store(false, Ordering::SeqCst);
        self.changed.store(false, Ordering::SeqCst);
    }
}
