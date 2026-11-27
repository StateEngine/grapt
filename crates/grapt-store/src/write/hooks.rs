//! Typed hook registration without exposing mutable connection authority.
use super::EventStore;
use crate::session_list::{install_vm_budget, SqliteVmBudget, VmBudgetGuard};
use rusqlite::hooks::Action;

impl EventStore {
    /// Bound synchronous reads while retaining the governed-write authorizer.
    pub fn read_budget(&mut self, budget: SqliteVmBudget) -> rusqlite::Result<VmBudgetGuard<'_>> {
        install_vm_budget(&mut self.conn, budget)
    }

    /// Observe or veto commits without replacing the governed-write authorizer.
    pub fn commit_hook<F>(&mut self, hook: Option<F>) -> rusqlite::Result<()>
    where
        F: FnMut() -> bool + Send + 'static,
    {
        self.conn.commit_hook(hook)
    }

    /// Observe row changes without replacing the governed-write authorizer.
    pub fn update_hook<F>(&mut self, hook: Option<F>) -> rusqlite::Result<()>
    where
        F: FnMut(Action, &str, &str, i64) + Send + 'static,
    {
        self.conn.update_hook(hook)
    }

    /// Observe rollback without replacing the governed-write authorizer.
    pub fn rollback_hook<F>(&mut self, hook: Option<F>) -> rusqlite::Result<()>
    where
        F: FnMut() + Send + 'static,
    {
        self.conn.rollback_hook(hook)
    }

    /// Register a query-progress observer under exclusive store ownership.
    pub fn progress_handler<F>(&mut self, num_ops: i32, handler: Option<F>) -> rusqlite::Result<()>
    where
        F: FnMut() -> bool + Send + 'static,
    {
        self.conn.progress_handler(num_ops, handler)
    }
}
