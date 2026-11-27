//! The durable history a live Step chain reads: its generation advances
//! with the Session's event log, and compaction runs through the product
//! compactor inside the owning Step's cancellation context.

use std::sync::{Arc, Mutex};

use grapt_inbox::Inbox;
use grapt_runtime::{CompactionError, HistoryGeneration, HistorySource};
use grapt_types::SessionId;

#[derive(Debug)]
pub(super) struct DurableHistory {
    pub(super) inbox: Arc<Mutex<Inbox>>,
    pub(super) session: SessionId,
    pub(super) generation: HistoryGeneration,
    pub(super) compactor: super::super::continuation::ProductCompactor,
}

impl HistorySource for DurableHistory {
    fn promote_input(&mut self, input: &grapt_types::MessageId) -> Result<(), String> {
        self.compactor.promote_input(input)
    }

    async fn compact(&mut self) -> Result<(), CompactionError> {
        Err(CompactionError {
            reason: "product compaction requires the owning Step cancellation context".to_owned(),
        })
    }

    async fn compact_with(
        &mut self,
        context: grapt_runtime::CompactionContext,
    ) -> Result<Result<(), grapt_runtime::CompactionFailure>, grapt_types::InterruptionSource> {
        self.compactor.compact(context).await
    }

    fn reload(&mut self) -> Result<HistoryGeneration, CompactionError> {
        let inbox = match self.inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let reloaded = head_generation(inbox.store(), &self.session)?;
        if reloaded <= self.generation {
            return Err(CompactionError {
                reason: format!(
                    "durable history generation {} did not advance past {}",
                    reloaded.value(),
                    self.generation.value()
                ),
            });
        }
        self.generation = reloaded;
        Ok(self.generation)
    }

    fn generation(&self) -> HistoryGeneration {
        self.generation
    }
}

/// The Session's durable head as a history generation.
///
/// The generation is this Session's own log: a row in another aggregate family
/// is not Session history even if it shares the identity string.
fn head_generation(
    store: &grapt_store::EventStore,
    session: &SessionId,
) -> Result<HistoryGeneration, CompactionError> {
    let latest = grapt_store::session_event_queries::bounds::head(store, session)
        .map_err(|error| CompactionError { reason: error })?
        .ok_or_else(|| CompactionError {
            reason: "durable history has no head to advance from".to_owned(),
        })?;
    Ok(HistoryGeneration::new(latest.value()))
}

#[cfg(test)]
#[path = "durable_history_tests.rs"]
mod durable_history_tests;
