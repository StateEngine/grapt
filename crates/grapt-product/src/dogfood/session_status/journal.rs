//! Commit authoritative observations before offering their revision to clients.
use super::*;
use grapt_service_state::{Change, Snapshot, State};
use grapt_types::{DeterministicAuthorities, ExecutionId};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Presentation {
    pub status: SessionStatusInfo,
    pub phase: Option<grapt_api::u3a::SessionStatusPhase>,
}

impl SessionStatusEmitter {
    pub(crate) fn with_journal(mut self, inbox: Arc<Mutex<grapt_inbox::Inbox>>) -> Self {
        self.journal = Some(inbox);
        self
    }

    /// Store callbacks cannot await. Presentation is confirmed again under
    /// the hub lock so a delayed offer cannot replace a newer legacy phase.
    pub(crate) fn observe_owned(
        &self,
        session: &SessionId,
        execution: &ExecutionId,
        change: Change,
        current: impl FnOnce(&grapt_inbox::Inbox) -> Result<Option<State>, String>,
        completed: impl FnOnce(bool),
        present: impl Fn(&Snapshot) -> Result<Presentation, String>,
    ) -> Result<(), String> {
        let Some(journal) = &self.journal else {
            return Ok(());
        };
        let snapshot = {
            let mut inbox = match journal.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            let state = match current(&inbox) {
                Ok(state) => state,
                Err(error) => {
                    completed(false);
                    return Err(error);
                }
            };
            let Some(state) = state else {
                return Ok(());
            };
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            let result = grapt_service_state::record(
                inbox.store_mut(),
                &authorities,
                session,
                execution,
                state,
                change,
            );
            let result = result.and_then(|snapshot| {
                if snapshot.state.active() && snapshot.execution.as_ref() != Some(execution) {
                    Err("session status producer no longer owns the execution".into())
                } else {
                    Ok(snapshot)
                }
            });
            completed(result.is_ok());
            result?
        };
        self.publish_snapshot(session, snapshot, present)
    }

    pub(crate) fn publish_current(
        &self,
        session: &SessionId,
        present: impl Fn(&Snapshot) -> Result<Presentation, String>,
    ) -> Result<(), String> {
        let Some(journal) = &self.journal else {
            return Ok(());
        };
        let snapshot = {
            let inbox = match journal.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            let snapshot = grapt_service_state::read(inbox.store().connection(), session)?;
            present(&snapshot)?;
            snapshot
        };
        self.publish_snapshot(session, snapshot, present)
    }
}

pub(crate) fn wire(snapshot: Snapshot) -> Result<grapt_api::u3a::SessionStateDto, String> {
    let mut value = serde_json::to_value(snapshot.state).map_err(|error| error.to_string())?;
    value["revision"] = serde_json::json!(snapshot.position.seq);
    if let Some(execution) = snapshot.execution {
        value["executionID"] = serde_json::json!(execution.as_str());
    }
    crate::dogfood::decode(value)
}
