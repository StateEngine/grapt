//! Hand off the scheduler core while the synchronous store commits on this thread.
use grapt_store::{commit_executor::CommitExecutor, EventStore};
use std::sync::Arc;

#[derive(Debug)]
struct RuntimeCommitExecutor;

impl CommitExecutor for RuntimeCommitExecutor {
    fn execute(&self, work: &mut dyn FnMut()) {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(work);
            }
            // Startup and synchronous/current-thread test owners have no
            // concurrent product HTTP host to hand off. Avoid Tokio's panic.
            _ => work(),
        }
    }
}

pub(crate) fn install(store: &mut EventStore) {
    store.install_commit_executor(Arc::new(RuntimeCommitExecutor));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronous_and_current_thread_owners_run_once() -> Result<(), Box<dyn std::error::Error>> {
        let mut calls = 0;
        RuntimeCommitExecutor.execute(&mut || calls += 1);
        let runtime = tokio::runtime::Builder::new_current_thread().build()?;
        runtime.block_on(async { RuntimeCommitExecutor.execute(&mut || calls += 1) });
        assert_eq!(calls, 2);
        Ok(())
    }

    #[test]
    fn multi_thread_owner_hands_off_without_moving_borrowed_work(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .build()?;
        let mut calls = 0;
        runtime.block_on(async { RuntimeCommitExecutor.execute(&mut || calls += 1) });
        assert_eq!(calls, 1);
        Ok(())
    }
}
