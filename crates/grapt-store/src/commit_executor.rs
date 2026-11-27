//! Composition selects scheduling; the durable store stays synchronous.
use std::{fmt::Debug, sync::Arc};

/// Runs the borrowed commit work synchronously, exactly once, before returning.
/// It cannot detach work or substitute a result. Runtime adapters may release
/// their scheduler core while the same thread owns the transaction.
pub trait CommitExecutor: Debug + Send + Sync {
    fn execute(&self, work: &mut dyn FnMut());
}

impl crate::EventStore {
    /// Applies to election and domain transactions opened after installation.
    /// Unconfigured stores execute inline, including current-thread test hosts.
    pub fn install_commit_executor(&mut self, executor: Arc<dyn CommitExecutor>) {
        self.commit_executor = Some(executor);
    }
}
